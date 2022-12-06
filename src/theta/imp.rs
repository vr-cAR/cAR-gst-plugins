use std::sync::{RwLock, Weak};
use std::time::Duration;
use std::{str::FromStr, sync::Arc};

use gstreamer::subclass::prelude::*;
use gstreamer::{glib, prelude::*};
use gstreamer_base::subclass::prelude::*;
use gstreamer_base::{prelude::*, subclass::base_src::CreateSuccess};
use once_cell::sync::Lazy;
use strum::IntoEnumIterator;
use strum_macros::{EnumIter, EnumString, IntoStaticStr};

use crate::{
    frame::FrameData,
    theta::libuvc_theta::{UvcContext, UvcDevice, UvcFrame, UvcStreamHandle},
};
use crate::{macros::set_field, theta::libuvc_theta::StreamParameters};

static CAT: Lazy<gstreamer::DebugCategory> = Lazy::new(|| {
    gstreamer::DebugCategory::new(
        "thetauvcsrc",
        gstreamer::DebugColorFlags::empty(),
        Some("Ricoh Theta Source"),
    )
});

const USBVID_RICOH: u16 = 0x05ca;
const USBPID_THETAV_UVC: u16 = 0x2712;
const USBPID_THETAZ1_UVC: u16 = 0x2715;

#[derive(Debug, Clone)]
struct Settings {
    width: u32,
    height: u32,
    fps: u32,
    mode: Mode,
    product: Product,
    serial_number: String,
    device_index: u32,
}

impl Default for Settings {
    fn default() -> Self {
        let mode = Mode::Fhd;
        let (mut width, mut height, mut fps) = (0, 0, 0);

        if let Some(preset) = mode.get_mode_settings() {
            width = preset.width;
            height = preset.height;
            fps = preset.fps;
        }
        Self {
            width,
            height,
            fps,
            device_index: 0,
            mode,
            product: Product::AnyProduct,
            serial_number: "".to_owned(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, EnumString, EnumIter, IntoStaticStr)]
#[strum(serialize_all = "kebab-case")]
enum SettingField {
    Width,
    Height,
    Fps,
    Mode,
    Product,
    SerialNumber,
    DeviceIndex,
}

struct State {
    device: Option<Arc<UvcDevice>>,
    stream: Option<UvcStreamHandle>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            device: None,
            stream: None,
        }
    }
}

pub struct ThetaUvc {
    settings: RwLock<Settings>,
    state: RwLock<State>,
    frame_data: RwLock<Option<Arc<FrameData<gstreamer::Buffer>>>>,
}

impl ThetaUvc {
    fn camera_caps() -> gstreamer::Caps {
        gstreamer::Caps::builder("video/x-h264")
            .field("stream-format", "byte-stream")
            .field("profile", "constrained-baseline")
            .field("alignment", "nal")
            .build()
    }
}

#[glib::object_subclass]
impl ObjectSubclass for ThetaUvc {
    const NAME: &'static str = "thetauvcsrc";

    type Type = super::ThetaUvc;

    type ParentType = gstreamer_base::PushSrc;

    fn new() -> Self {
        Self {
            settings: RwLock::new(Settings::default()),
            state: RwLock::new(State::default()),
            frame_data: RwLock::new(None),
        }
    }
}

impl ObjectImpl for ThetaUvc {
    fn properties() -> &'static [glib::ParamSpec] {
        static PROPERTIES: Lazy<Vec<glib::ParamSpec>> = Lazy::new(|| {
            SettingField::iter().map(|setting| {
                match setting {
                    SettingField::Width => {
                        glib::ParamSpecUInt::builder(SettingField::Width.into())
                            .nick("Camera Width")
                            .blurb("The width of the camera stream")
                            .build()
                    },
                    SettingField::Height => {
                        glib::ParamSpecUInt::builder(SettingField::Height.into())
                            .nick("Camera Height")
                            .blurb("The height of the camera stream")
                            .build()
                    },
                    SettingField::Fps => {
                        glib::ParamSpecUInt::builder(SettingField::Fps.into())
                            .nick("Camera FPS")
                            .blurb("The FPS to read from the camera")
                            .build()
                    },
                    SettingField::Mode => {
                        glib::ParamSpecEnum::builder(SettingField::Mode.into(), Settings::default().mode)
                            .nick("Stream Mode Presets")
                            .blurb("Which preset to use for streaming")
                            .build()
                    },
                    SettingField::Product => {
                        glib::ParamSpecEnum::builder(SettingField::Product.into(), Settings::default().product)
                            .nick("Ricoh Theta Product")
                            .blurb("The product type of the camera")
                            .build()
                    },
                    SettingField::SerialNumber => {
                        glib::ParamSpecString::builder(SettingField::SerialNumber.into())
                            .nick("Device Serial Number")
                            .blurb("The serial number of the device")
                            .build()
                    },
                    SettingField::DeviceIndex => {
                        glib::ParamSpecUInt::builder(SettingField::DeviceIndex.into())
                            .nick("Device Index")
                            .blurb("Given a list of devices that matches the capabilities of the desired device, chooses which one to use")
                            .build()
                    },
                }
            }).collect()
        });
        PROPERTIES.as_ref()
    }

    fn constructed(&self) {
        self.parent_constructed();
        let obj = self.instance();
        obj.set_format(gstreamer::Format::Time);
    }

    fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
        match SettingField::from_str(pspec.name()) {
            Ok(field) => {
                let mut settings = self.settings.write().unwrap();
                match field {
                    SettingField::Width => {
                        set_field!(CAT, self, field, settings.width, value);
                        set_field!(CAT, self, field, enum settings.mode, (Mode::NoMode));
                    }
                    SettingField::Height => {
                        set_field!(CAT, self, field, settings.height, value);
                        set_field!(CAT, self, field, enum settings.mode, (Mode::NoMode));
                    }
                    SettingField::Fps => {
                        set_field!(CAT, self, field, settings.fps, value);
                        set_field!(CAT, self, field, enum settings.mode, (Mode::NoMode));
                    }
                    SettingField::Mode => {
                        set_field!(CAT, self, field, enum settings.mode, value);
                        if let Some(preset) = settings.mode.get_mode_settings() {
                            set_field!(CAT, self, field, settings.width, (preset.width));
                            set_field!(CAT, self, field, settings.height, (preset.height));
                            set_field!(CAT, self, field, settings.fps, (preset.fps));
                        }
                    }
                    SettingField::Product => {
                        set_field!(CAT, self, field, enum settings.product, value)
                    }
                    SettingField::DeviceIndex => {
                        set_field!(CAT, self, field, settings.device_index, value)
                    }
                    SettingField::SerialNumber => {
                        set_field!(CAT, self, field, settings.serial_number, value)
                    }
                };
            }
            Err(_err) => {
                gstreamer::error!(CAT, imp: self, "Unknown field {}", pspec.name());
                panic!("Unknown field {}", pspec.name());
            }
        }
    }

    fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
        match SettingField::from_str(pspec.name()) {
            Ok(field) => {
                let settings = self.settings.read().unwrap();
                match field {
                    SettingField::Width => settings.width.to_value(),
                    SettingField::Height => settings.height.to_value(),
                    SettingField::Fps => settings.fps.to_value(),
                    SettingField::DeviceIndex => settings.device_index.to_value(),
                    SettingField::Mode => settings.mode.to_value(),
                    SettingField::Product => settings.product.to_value(),
                    SettingField::SerialNumber => settings.serial_number.to_value(),
                }
            }
            Err(_err) => {
                panic!("Unknown field {}", pspec.name())
            }
        }
    }
}

impl GstObjectImpl for ThetaUvc {}

impl ElementImpl for ThetaUvc {
    fn metadata() -> Option<&'static gstreamer::subclass::ElementMetadata> {
        static ELEMENT_METADATA: Lazy<gstreamer::subclass::ElementMetadata> = Lazy::new(|| {
            gstreamer::subclass::ElementMetadata::new(
                "Ricoh Theta Source",
                "Source/Video",
                "Ricoh Theta Source",
                "William Zhang <wtzhang23@gmail.com>",
            )
        });

        Some(&*ELEMENT_METADATA)
    }

    fn pad_templates() -> &'static [gstreamer::PadTemplate] {
        static PAD_TEMPLATES: Lazy<Vec<gstreamer::PadTemplate>> = Lazy::new(|| {
            let src_pad_template = gstreamer::PadTemplate::new(
                "src",
                gstreamer::PadDirection::Src,
                gstreamer::PadPresence::Always,
                &ThetaUvc::camera_caps(),
            )
            .unwrap();
            vec![src_pad_template]
        });
        PAD_TEMPLATES.as_ref()
    }

    fn change_state(
        &self,
        transition: gstreamer::StateChange,
    ) -> Result<gstreamer::StateChangeSuccess, gstreamer::StateChangeError> {
        match transition {
            gstreamer::StateChange::NullToReady => {
                let settings = {
                    let settings = self.settings.read().unwrap();
                    settings.clone()
                };
                let context = UvcContext::new().map_err(|err| {
                    gstreamer::element_imp_error!(
                        self,
                        gstreamer::LibraryError::Init,
                        ("Could not create a libuvc context. Error: {:#?}", err)
                    );
                    gstreamer::StateChangeError
                })?;

                let vid = Some(USBVID_RICOH).map(|i| i as i32);
                let pid = match settings.product {
                    Product::Z1 => Some(USBPID_THETAZ1_UVC),
                    Product::V => Some(USBPID_THETAV_UVC),
                    Product::AnyProduct => None,
                }
                .map(|i| i as i32);
                let serial_number = match settings.serial_number.as_str() {
                    "" => None,
                    sn => Some(sn),
                };
                let device = context
                    .find_devices(vid, pid, serial_number)
                    .map_err(|err| {
                        gstreamer::element_imp_error!(
                            self,
                            gstreamer::LibraryError::Init,
                            (
                                "Could not find device {}. Error: {:#?}",
                                [
                                    ("vid", vid.map(|vid| vid.to_string())),
                                    ("pid", pid.map(|pid| pid.to_string())),
                                    ("serial number", serial_number.map(|sn| sn.to_owned()))
                                ]
                                .into_iter()
                                .map(|(name, val)| format!(
                                    "{} = {}",
                                    name,
                                    match val {
                                        Some(val) => val,
                                        None => "<any>".to_owned(),
                                    }
                                ))
                                .collect::<Vec<_>>()
                                .join(","),
                                err
                            )
                        );
                        gstreamer::StateChangeError
                    })?;
                if let Some(device) = device.into_iter().nth(settings.device_index as usize) {
                    let mut state = self.state.write().unwrap();
                    state.device.replace(Arc::new(device));
                } else {
                    gstreamer::element_imp_error!(
                        self,
                        gstreamer::LibraryError::Init,
                        ("Provided device index {} greater than the number of devices that exist that meets the requested specifications", settings.device_index)
                    );
                    return Err(gstreamer::StateChangeError);
                }
            }
            gstreamer::StateChange::ReadyToNull => {
                let mut state = self.state.write().unwrap();
                state.device.take();
            }
            _ => (),
        }

        self.parent_change_state(transition)
    }
}

impl BaseSrcImpl for ThetaUvc {
    fn negotiate(&self) -> Result<(), gstreamer::LoggableError> {
        if let Some(caps) = self.caps(None) {
            self.instance()
                .set_caps(&caps)
                .map_err(|_| gstreamer::loggable_error!(CAT, "Failed to negotiate caps",))
        } else {
            Err(gstreamer::loggable_error!(CAT, "Failed to negotiate caps",))
        }
    }

    fn caps(&self, filter: Option<&gstreamer::Caps>) -> Option<gstreamer::Caps> {
        let caps = Self::camera_caps();
        if let Some(filter) = filter {
            if filter.can_intersect(&caps) {
                Some(caps.intersect(filter))
            } else {
                None
            }
        } else {
            Some(caps)
        }
    }

    fn start(&self) -> Result<(), gstreamer::ErrorMessage> {
        let settings = {
            // settings span
            let settings = self.settings.read().unwrap();
            settings.clone()
        };

        let frame_data = {
            // frame data span
            let mut frame_data = self.frame_data.write().unwrap();
            let fd = Arc::new(FrameData::default());
            let weak = Arc::downgrade(&fd);
            frame_data.replace(fd);
            weak
        };

        let mut state = self.state.write().unwrap();
        let device = state.device.as_ref().ok_or_else(|| {
            gstreamer::error_msg!(
                gstreamer::LibraryError::Init,
                ["device not initialized yet"]
            )
        })?;
        let device_handle = device.open().map_err(|err| {
            gstreamer::error_msg!(
                gstreamer::CoreError::Failed,
                ("Could not open device. Error: {:#?}", err)
            )
        })?;

        gstreamer::info!(
            CAT,
            imp: self,
            "Starting camera stream with width={},height={},fps={}",
            settings.width,
            settings.height,
            settings.fps
        );

        let stream_handle = device_handle
            .start_streaming(
                StreamParameters {
                    width: settings.width as usize,
                    height: settings.height as usize,
                    fps: settings.fps as usize,
                },
                on_frame_callback,
                frame_data,
            )
            .map_err(|err| {
                gstreamer::error_msg!(
                    gstreamer::LibraryError::Init,
                    ("Cannot open device to begin streaming. Error: {:#?}", err)
                )
            })?;
        state.stream.replace(stream_handle);

        Ok(())
    }

    fn stop(&self) -> Result<(), gstreamer::ErrorMessage> {
        {
            let mut state = self.state.write().unwrap();
            state.stream.take();
        }
        {
            self.frame_data.write().unwrap().take();
        }
        Ok(())
    }

    fn query(&self, query: &mut gstreamer::QueryRef) -> bool {
        match query.view_mut() {
            gstreamer::QueryViewMut::Latency(latency) => {
                latency.set(
                    true,
                    self.frame_data
                        .read()
                        .unwrap()
                        .as_ref()
                        .and_then(|fd| fd.get_latency())
                        .map_or_else(
                            || {
                                gstreamer::ClockTime::from_nseconds(
                                    Duration::from_secs_f64(
                                        1001f64 / (self.settings.read().unwrap().fps * 1000) as f64,
                                    )
                                    .as_nanos() as u64,
                                )
                            },
                            |stream| {
                                gstreamer::info!(
                                    CAT,
                                    imp: self,
                                    "Responding to query with live latency"
                                );
                                gstreamer::ClockTime::from_nseconds(stream.as_nanos() as u64)
                            },
                        ),
                    Some(gstreamer::ClockTime::from_mseconds(100)),
                );
                true
            }
            gstreamer::QueryViewMut::Caps(caps) => {
                caps.set_result(&self.caps(caps.filter().map(|r| r.to_owned()).as_ref()));
                true
            }
            _ => BaseSrcImplExt::parent_query(self, query),
        }
    }
}

impl PushSrcImpl for ThetaUvc {
    fn create(
        &self,
        _buffer: Option<&mut gstreamer::BufferRef>,
    ) -> Result<CreateSuccess, gstreamer::FlowError> {
        let mut frame_data = self.frame_data.read().unwrap();
        while let Some(fd) = frame_data.as_ref() {
            if let Some(buffer) = fd.pop_frame() {
                gstreamer::debug!(
                    CAT,
                    imp: self,
                    "Got frame from camera. pts={:#?}, dts={:#?}, duration={:#?}",
                    buffer.as_ref().pts(),
                    buffer.as_ref().dts(),
                    buffer.as_ref().duration()
                );
                return Ok(CreateSuccess::NewBuffer(buffer));
            }
            gstreamer::debug!(CAT, imp: self, "Sleeping until next frame");
            let fd = fd.clone();
            std::mem::drop(frame_data); // let another person use lock
            fd.wait();
            gstreamer::debug!(CAT, imp: self, "Woke up");
            frame_data = self.frame_data.read().unwrap();
        }
        Err(gstreamer::FlowError::Eos)
    }
}

#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Clone, Copy, glib::Enum)]
#[repr(u32)]
#[enum_type(name = "GstThetaUvcMode")]
pub enum Mode {
    Uhd,
    Fhd,
    NoMode,
}

struct ModeSettings {
    width: u32,
    height: u32,
    fps: u32,
}

impl Mode {
    fn get_mode_settings(&self) -> Option<ModeSettings> {
        match self {
            Mode::Uhd => Some(ModeSettings {
                width: 3840,
                height: 1920,
                fps: 29,
            }),
            Mode::Fhd => Some(ModeSettings {
                width: 1920,
                height: 960,
                fps: 29,
            }),
            Mode::NoMode => None,
        }
    }
}

fn on_frame_callback(
    frame: UvcFrame,
    frame_data: &mut Weak<FrameData<gstreamer::Buffer>>,
    stream_handle: &UvcStreamHandle,
) {
    let Some(frame_data) = frame_data.upgrade() else {
        return;
    };
    gstreamer::debug!(CAT, "Creating buffer for frame");
    let Ok(mut buffer) = gstreamer::Buffer::with_size(frame.data().len()) else {
        return;
    };
    {
        let Ok(mut mapped_writable) = buffer.make_mut().map_writable() else {
            return;
        };
        mapped_writable.as_mut_slice().copy_from_slice(frame.data());
    }

    let span = {
        let start_timestamp = frame_data.start_timestamp(frame.finish_timestamp());
        let pts = frame.finish_timestamp().saturating_sub(start_timestamp);
        let duration = stream_handle.frame_interval();

        buffer
            .make_mut()
            .set_pts(Some(gstreamer::ClockTime::from_nseconds(
                pts.as_nanos().try_into().unwrap(),
            )));
        buffer.make_mut().set_dts(None);
        buffer
            .make_mut()
            .set_duration(Some(gstreamer::ClockTime::from_nseconds(
                duration.as_nanos().try_into().unwrap(),
            )));
        buffer.make_mut().set_offset(frame.sequence() as u64);
        frame_data.add_frame(buffer);
        duration
    };

    frame_data.update_latency(span);
}

#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Clone, Copy, glib::Enum)]
#[repr(u32)]
#[enum_type(name = "GstThetaUvcProduct")]
pub enum Product {
    Z1,
    V,
    AnyProduct,
}
