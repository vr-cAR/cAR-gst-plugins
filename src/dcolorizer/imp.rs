use rayon::prelude::*;
use std::{
    str::FromStr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex, RwLock,
    },
};

use gstreamer::{glib, prelude::*, subclass::prelude::*};
use gstreamer_base::subclass::{
    prelude::{BaseTransformImpl, BaseTransformImplExt},
    BaseTransformMode,
};
use gstreamer_video::{VideoFormat, VideoInfo};
use once_cell::sync::Lazy;
use strum::IntoEnumIterator;
use strum_macros::{EnumIter, EnumString, IntoStaticStr};

use crate::macros::set_field;

static CAT: Lazy<gstreamer::DebugCategory> = Lazy::new(|| {
    gstreamer::DebugCategory::new(
        "dcolorizer",
        gstreamer::DebugColorFlags::empty(),
        Some("De(pth)-Colorizer"),
    )
});

#[derive(Clone, Copy, Debug, PartialEq, EnumString, EnumIter, IntoStaticStr)]
#[strum(serialize_all = "kebab-case")]
enum SettingField {
    Threads,
    MinDepth,
    MaxDepth,
}

struct Settings {
    threads: u32,
    min_depth: u32,
    max_depth: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            threads: 0,
            min_depth: 1,
            max_depth: u16::MAX as u32,
        }
    }
}

struct State {
    thread_pool: Option<rayon::ThreadPool>,
    sink_caps_to_use: Option<gstreamer::Caps>,
    src_caps_to_use: Option<gstreamer::Caps>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            thread_pool: None,
            sink_caps_to_use: None,
            src_caps_to_use: None,
        }
    }
}

pub struct DColorizer {
    settings: RwLock<Settings>,
    state: Mutex<State>,
}

impl DColorizer {
    fn depth_caps() -> &'static gstreamer::Caps {
        static DEPTH_CAPS: Lazy<gstreamer::Caps> = Lazy::new(|| {
            [
                VideoFormat::Gray16Le.to_str().to_owned(),
                VideoFormat::Gray16Be.to_str().to_owned(),
            ]
            .into_iter()
            .map(|format| {
                gstreamer::Caps::builder("video/x-raw")
                    .field("format", format)
                    .build()
            })
            .reduce(|mut acc, element| {
                acc.merge(element);
                acc
            })
            .unwrap()
        });
        &*DEPTH_CAPS
    }

    fn color_caps() -> &'static gstreamer::Caps {
        static COLOR_CAPS: Lazy<gstreamer::Caps> = Lazy::new(|| {
            [VideoFormat::Rgb.to_str().to_owned()]
                .into_iter()
                .map(|format| {
                    gstreamer::Caps::builder("video/x-raw")
                        .field("format", format)
                        .field("framerate", gstreamer::Fraction::new(0, 1))
                        .build()
                })
                .reduce(|mut acc, element| {
                    acc.merge(element);
                    acc
                })
                .unwrap()
        });
        &*COLOR_CAPS
    }

    fn all_caps() -> &'static gstreamer::Caps {
        static ALL_CAPS: Lazy<gstreamer::Caps> = Lazy::new(|| {
            let mut caps = DColorizer::depth_caps().to_owned();
            caps.merge(DColorizer::color_caps().to_owned());
            caps
        });
        &*ALL_CAPS
    }

    fn colorize(&self, width: usize, height: usize, from: &[u8], to: &mut [u8], big_endian: bool) {
        let (min_depth, max_depth) = {
            let settings = self.settings.read().unwrap();
            (
                settings.min_depth as u16,
                settings.max_depth.min(u16::MAX as u32) as u16,
            )
        };

        let (min_disparity, max_disparity) = { (1f64 / max_depth as f64, 1f64 / min_depth as f64) };

        let counter = AtomicUsize::new(0);
        from.par_chunks_exact(2)
            .zip(to.par_chunks_exact_mut(3))
            .for_each(|(depth, pixel)| {
                assert_eq!(depth.len(), 2);
                assert_eq!(pixel.len(), 3);
                let depth = if big_endian {
                    ((depth[0] as u16) << 8) | (depth[1] as u16)
                } else {
                    (depth[0] as u16) | ((depth[1] as u16) << 8)
                };
                let disparity = match depth {
                    0 => max_disparity,
                    _ => 1f64 / (depth as f64),
                }
                .clamp(min_disparity, max_disparity);

                let d_normal = 1529f64 * (disparity - min_disparity) as f64
                    / (max_disparity - min_disparity) as f64;
                let d_normal: isize = d_normal as isize;
                let r: isize = match d_normal {
                    0..=255 | 1276..=1529 => 255,
                    256..=510 => 255 - d_normal,
                    511..=1020 => 0,
                    1021..=1275 => d_normal - 1020,
                    _ => unreachable!(),
                };

                let g: isize = match d_normal {
                    0..=255 => d_normal,
                    256..=510 => 255,
                    511..=765 => 765 - d_normal,
                    766..=1529 => 0,
                    _ => unreachable!(),
                };

                let b: isize = match d_normal {
                    0..=765 => 0,
                    766..=1020 => d_normal - 765,
                    1021..=1275 => 255,
                    1276..=1529 => 1529 - d_normal,
                    _ => unreachable!(),
                };

                let r = r & u8::MAX as isize;
                let g = g & u8::MAX as isize;
                let b = b & u8::MAX as isize;
                pixel[0] = r as u8;
                pixel[1] = g as u8;
                pixel[2] = b as u8;
                counter.fetch_add(1, Ordering::Release);
            })
            .join();
        assert_eq!(counter.load(Ordering::Acquire), width * height);
    }

    fn decolorize(
        &self,
        width: usize,
        height: usize,
        from: &[u8],
        to: &mut [u8],
        big_endian: bool,
    ) {
        let (min_depth, max_depth) = {
            let settings = self.settings.read().unwrap();
            (
                settings.min_depth as u16,
                settings.max_depth.min(u16::MAX as u32) as u16,
            )
        };

        let (min_disparity, max_disparity) = { (1f64 / max_depth as f64, 1f64 / min_depth as f64) };

        let counter = AtomicUsize::new(0);
        from.par_chunks_exact(3)
            .zip(to.par_chunks_exact_mut(2))
            .for_each(|(depth, pixel)| {
                // depth is little endian
                let r = depth[0] as usize;
                let g = depth[1] as usize;
                let b = depth[2] as usize;
                let dnormal = if (r + g + b) < 255 {
                    0
                } else if r >= g && r >= b {
                    if g >= b {
                        g - b
                    } else {
                        g - b + 1529
                    }
                } else if g >= r && g >= b {
                    b - r + 510
                } else if b >= g && b >= r {
                    r - g + 1020
                } else {
                    0
                };

                let disparity =
                    min_disparity + (max_disparity - min_disparity) * (dnormal as f64 / 1529f64);
                let depth = 1f64 / disparity;
                let depth = (depth as usize).clamp(0, u16::MAX as usize) as u16;
                let as_bytes = if big_endian {
                    depth.to_be_bytes()
                } else {
                    depth.to_le_bytes()
                };
                pixel.copy_from_slice(&as_bytes);
                counter.fetch_add(1, Ordering::Release);
            })
            .join();
        assert_eq!(counter.load(Ordering::Acquire), width * height);
    }
}

#[glib::object_subclass]
impl ObjectSubclass for DColorizer {
    const NAME: &'static str = "dcolorizer";
    type Type = super::DColorizer;
    type ParentType = gstreamer_base::BaseTransform;

    fn new() -> Self {
        Self {
            settings: RwLock::new(Settings::default()),
            state: Mutex::new(State::default()),
        }
    }
}

impl ObjectImpl for DColorizer {
    fn properties() -> &'static [glib::ParamSpec] {
        static PROPERTIES: Lazy<Vec<glib::ParamSpec>> = Lazy::new(|| {
            SettingField::iter()
                .map(|setting| match setting {
                    SettingField::Threads => glib::ParamSpecUInt::builder(setting.into())
                        .nick("Threads")
                        .blurb("Number of threads to use for colorization (0 for automatic)")
                        .build(),
                    SettingField::MinDepth => glib::ParamSpecUInt::builder(setting.into())
                        .nick("Min Depth")
                        .blurb("The minimum depth to clamp")
                        .build(),
                    SettingField::MaxDepth => glib::ParamSpecUInt::builder(setting.into())
                        .nick("Max Depth")
                        .blurb("The maximum depth to clamp")
                        .build(),
                })
                .collect()
        });
        PROPERTIES.as_ref()
    }

    fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
        match SettingField::from_str(pspec.name()) {
            Ok(field) => {
                let mut settings = self.settings.write().unwrap();
                match field {
                    SettingField::Threads => {
                        set_field!(CAT, self, field, settings.threads, value);
                    }
                    SettingField::MinDepth => {
                        set_field!(CAT, self, field, settings.min_depth, value);
                    }
                    SettingField::MaxDepth => {
                        set_field!(CAT, self, field, settings.max_depth, value);
                    }
                }
            }
            Err(_err) => {
                panic!("Unknown field {}", pspec.name());
            }
        }
    }

    fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
        match SettingField::from_str(pspec.name()) {
            Ok(field) => {
                let settings = self.settings.read().unwrap();
                match field {
                    SettingField::Threads => settings.threads.to_value(),
                    SettingField::MinDepth => settings.min_depth.to_value(),
                    SettingField::MaxDepth => settings.max_depth.to_value(),
                }
            }
            Err(_err) => {
                panic!("Unknown field {}", pspec.name());
            }
        }
    }
}

impl GstObjectImpl for DColorizer {}

impl ElementImpl for DColorizer {
    fn metadata() -> Option<&'static gstreamer::subclass::ElementMetadata> {
        static ELEMENT_METADATA: Lazy<gstreamer::subclass::ElementMetadata> = Lazy::new(|| {
            gstreamer::subclass::ElementMetadata::new(
                "De(pth)-Colorizer",
                "Source/Video",
                "Depth Colorizer and Decolorizer",
                "William Zhang <wtzhang23@gmail.com>",
            )
        });

        Some(&*ELEMENT_METADATA)
    }

    fn pad_templates() -> &'static [gstreamer::PadTemplate] {
        static PAD_TEMPLATES: Lazy<Vec<gstreamer::PadTemplate>> = Lazy::new(|| {
            [
                ("src", gstreamer::PadDirection::Src),
                ("sink", gstreamer::PadDirection::Sink),
            ]
            .into_iter()
            .map(|(name, direction)| {
                gstreamer::PadTemplate::new(
                    name,
                    direction,
                    gstreamer::PadPresence::Always,
                    &DColorizer::all_caps(),
                )
                .unwrap()
            })
            .collect()
        });
        PAD_TEMPLATES.as_ref()
    }

    fn change_state(
        &self,
        transition: gstreamer::StateChange,
    ) -> Result<gstreamer::StateChangeSuccess, gstreamer::StateChangeError> {
        match transition {
            gstreamer::StateChange::NullToReady => {
                let threads = self.settings.read().unwrap().threads;
                self.state.lock().unwrap().thread_pool.replace(
                    rayon::ThreadPoolBuilder::new()
                        .num_threads(threads as usize)
                        .thread_name(|idx| format!("dcolorizer-{}", idx))
                        .build()
                        .map_err(|err| {
                            gstreamer::element_imp_error!(
                                self,
                                gstreamer::LibraryError::Init,
                                ("Could not initiate thread pool. Error: {:#?}", err)
                            );
                            gstreamer::StateChangeError
                        })?,
                );
            }
            gstreamer::StateChange::ReadyToNull => {
                self.state.lock().unwrap().thread_pool.take();
            }
            _ => (),
        }
        self.parent_change_state(transition)
    }
}

impl BaseTransformImpl for DColorizer {
    const MODE: BaseTransformMode = BaseTransformMode::Both;

    const PASSTHROUGH_ON_SAME_CAPS: bool = true;

    const TRANSFORM_IP_ON_PASSTHROUGH: bool = true;

    fn unit_size(&self, caps: &gstreamer::Caps) -> Option<usize> {
        let video_info = VideoInfo::from_caps(&caps).ok();
        video_info.map(|vi| vi.size())
    }

    fn set_caps(
        &self,
        incaps: &gstreamer::Caps,
        outcaps: &gstreamer::Caps,
    ) -> Result<(), gstreamer::LoggableError> {
        if self.accept_caps(gstreamer::PadDirection::Sink, incaps)
            || !self.accept_caps(gstreamer::PadDirection::Src, outcaps)
        {
            let mut state = self.state.lock().unwrap();
            state.sink_caps_to_use.replace(incaps.to_owned());
            state.src_caps_to_use.replace(outcaps.to_owned());
            BaseTransformImplExt::parent_set_caps(self, incaps, outcaps)
        } else {
            Err(gstreamer::loggable_error!(
                CAT,
                "set caps are not compatible"
            ))
        }
    }

    fn accept_caps(&self, _direction: gstreamer::PadDirection, caps: &gstreamer::Caps) -> bool {
        caps.can_intersect(&Self::all_caps())
    }

    fn query(&self, direction: gstreamer::PadDirection, query: &mut gstreamer::QueryRef) -> bool {
        match query.view_mut() {
            gstreamer::QueryViewMut::Caps(caps_query) => {
                match caps_query.filter() {
                    Some(caps) => {
                        let result = if self.accept_caps(direction, &caps.to_owned()) {
                            Some(caps.intersect(&Self::all_caps()))
                        } else {
                            None
                        };
                        caps_query.set_result(result.as_ref());
                        match direction {
                            gstreamer::PadDirection::Unknown => {}
                            gstreamer::PadDirection::Src => {
                                self.state.lock().unwrap().src_caps_to_use = result;
                            }
                            gstreamer::PadDirection::Sink => {
                                self.state.lock().unwrap().sink_caps_to_use = result;
                            }
                            _ => todo!(),
                        }
                    }
                    None => {
                        caps_query.set_result(Some(Self::all_caps()));
                    }
                }
                true
            }
            _ => BaseTransformImplExt::parent_query(self, direction, query),
        }
    }

    fn transform_caps(
        &self,
        _direction: gstreamer::PadDirection,
        caps: &gstreamer::Caps,
        _filter: Option<&gstreamer::Caps>,
    ) -> Option<gstreamer::Caps> {
        let Ok(video_info) = VideoInfo::from_caps(caps) else {
            return None;
        };
        let (width, height) = (video_info.width(), video_info.height());
        match video_info.format() {
            VideoFormat::Rgb => {
                let Some(depth_compatible) = VideoInfo::builder(VideoFormat::Gray16Be, width, height).build().ok().and_then(|vi| vi.to_caps().ok()) else {
                    return None;
                };
                let Some(color_compatible) = VideoInfo::builder(VideoFormat::Gray16Be, width, height).build().ok().and_then(|vi| vi.to_caps().ok()) else {
                    return None;
                };
                [depth_compatible, color_compatible]
                    .into_iter()
                    .reduce(|mut acc, element| {
                        acc.merge(element);
                        acc
                    })
            }
            gray @ (VideoFormat::Gray16Le | VideoFormat::Gray16Be) => {
                let Some(depth_compatible) = VideoInfo::builder(gray, width, height).build().ok().and_then(|vi| vi.to_caps().ok()) else {
                    return None;
                };
                let Some(color_compatible) = VideoInfo::builder(VideoFormat::Rgb, width, height).build().ok().and_then(|vi| vi.to_caps().ok()) else {
                    return None;
                };
                [depth_compatible, color_compatible]
                    .into_iter()
                    .reduce(|mut acc, element| {
                        acc.merge(element);
                        acc
                    })
            }
            _ => None,
        }
    }

    fn transform(
        &self,
        inbuf: &gstreamer::Buffer,
        outbuf: &mut gstreamer::BufferRef,
    ) -> Result<gstreamer::FlowSuccess, gstreamer::FlowError> {
        let state = self.state.lock().unwrap();
        let Some(sink_info) = state.sink_caps_to_use.as_ref().and_then(|caps| VideoInfo::from_caps(caps).ok()) else {
            gstreamer::element_imp_error!(
                self,
                gstreamer::CoreError::Negotiation,
                ("Sink not negotiated yet")
            );
            return Err(gstreamer::FlowError::Error);
        };
        let Some(src_info) = state.src_caps_to_use.as_ref().and_then(|caps| VideoInfo::from_caps(caps).ok()) else {
            gstreamer::element_imp_error!(
                self,
                gstreamer::CoreError::Negotiation,
                ("Src not negotiated yet")
            );
            return Err(gstreamer::FlowError::Error);
        };

        assert!(
            sink_info.format() != src_info.format(),
            "Passthrough should be in place"
        );

        // clone timestamps
        outbuf.set_pts(inbuf.pts());
        outbuf.set_dts(inbuf.dts());
        outbuf.set_duration(inbuf.duration());
        outbuf.set_offset(inbuf.offset());

        let inbuf_readable = inbuf.map_readable().map_err(|_err| {
            gstreamer::element_imp_error!(
                self,
                gstreamer::LibraryError::Encode,
                ("Could not open buffer for reading")
            );
            gstreamer::FlowError::Error
        })?;

        let mut outbuf_writable = outbuf.map_writable().map_err(|_err| {
            gstreamer::element_imp_error!(
                self,
                gstreamer::LibraryError::Encode,
                ("Could not open buffer for writing")
            );
            gstreamer::FlowError::Error
        })?;

        match sink_info.format() {
            VideoFormat::Rgb => {
                let be = match src_info.format() {
                    VideoFormat::Gray16Le => false,
                    VideoFormat::Gray16Be => true,
                    _ => unreachable!(),
                };
                assert_eq!(sink_info.width() * 3, sink_info.stride()[0] as u32);
                assert_eq!(src_info.width() * 2, src_info.stride()[0] as u32);
                assert_eq!(
                    sink_info.width() * sink_info.height() * 3,
                    inbuf_readable.len() as u32
                );
                assert_eq!(
                    sink_info.width() * sink_info.height() * 2,
                    outbuf_writable.len() as u32
                );
                self.decolorize(
                    sink_info.width() as usize,
                    sink_info.height() as usize,
                    &inbuf_readable,
                    outbuf_writable.as_mut_slice(),
                    be,
                );
            }
            gray @ (VideoFormat::Gray16Le | VideoFormat::Gray16Be) => {
                assert_eq!(sink_info.width() * 2, sink_info.stride()[0] as u32);
                assert_eq!(src_info.width() * 3, src_info.stride()[0] as u32);
                assert_eq!(
                    sink_info.width() * sink_info.height() * 2,
                    inbuf_readable.len() as u32
                );
                assert_eq!(
                    sink_info.width() * sink_info.height() * 3,
                    outbuf_writable.len() as u32
                );
                let be = gray == VideoFormat::Gray16Be;
                self.colorize(
                    sink_info.width() as usize,
                    sink_info.height() as usize,
                    &inbuf_readable,
                    outbuf_writable.as_mut_slice(),
                    be,
                );
            }
            _ => unreachable!(),
        }

        Ok(gstreamer::FlowSuccess::Ok)
    }

    fn transform_ip(
        &self,
        _buf: &mut gstreamer::BufferRef,
    ) -> Result<gstreamer::FlowSuccess, gstreamer::FlowError> {
        Ok(gstreamer::FlowSuccess::Ok)
    }
}
