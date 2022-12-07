use std::sync::Arc;
use std::{str::FromStr, sync::RwLock, time::Duration};

use gstreamer::{glib, prelude::*, subclass::prelude::*, Caps};
use gstreamer_base::{
    subclass::{base_src::CreateSuccess, prelude::*},
    traits::BaseSrcExt,
};
use gstreamer_video::VideoFormat;
use once_cell::sync::Lazy;
use strum::IntoEnumIterator;
use strum_macros::{EnumIter, EnumString, IntoStaticStr};

use crate::{frame::FrameData, k4a::libk4a, macros::set_field};

use super::libk4a::{Device, Stream};

static CAT: Lazy<gstreamer::DebugCategory> = Lazy::new(|| {
    gstreamer::DebugCategory::new(
        "k4asrc",
        gstreamer::DebugColorFlags::empty(),
        Some("Azure Kinect Source"),
    )
});

#[derive(Debug, Clone)]
struct Settings {
    fps_mode: FpsMode,
    color_resolution: ColorResolution,
    depth_mode: DepthMode,
    mode: Mode,
}

impl Settings {
    const IR_PASSIVE_RESOLUTION: (i32, i32) = (1024, 1024);

    fn resolution(&self) -> (i32, i32) {
        match self.mode {
            Mode::Color => self.color_resolution.dimensions(),
            Mode::Ir => Self::IR_PASSIVE_RESOLUTION,
            Mode::Depth => self.depth_mode.dimensions(),
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            fps_mode: FpsMode::Fps30,
            color_resolution: ColorResolution::Res720P,
            depth_mode: DepthMode::NormalFov2x2Binned,
            mode: Mode::Depth,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, EnumString, EnumIter, IntoStaticStr)]
#[strum(serialize_all = "kebab-case")]
enum SettingField {
    Fps,
    ColorResolution,
    DepthMode,
    Mode,
}

enum StreamState {
    Open(Stream),
    Closed(Device),
}

struct State {
    camera: Option<StreamState>,
    cap_to_use: Option<gstreamer::Caps>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            camera: None,
            cap_to_use: None,
        }
    }
}

pub struct K4a {
    settings: RwLock<Settings>,
    state: RwLock<State>,
    frame_data: Arc<FrameData<()>>,
}

impl K4a {
    fn color_caps() -> gstreamer::Caps {
        gstreamer::Caps::builder_full()
            .structure(
                gstreamer::Structure::builder("video/x-raw")
                    .field("format", VideoFormat::Bgra.to_str().to_owned())
                    .build(),
            )
            .structure(
                gstreamer::Structure::builder("video/x-raw")
                    .field("format", VideoFormat::Yuy2.to_str().to_owned())
                    .build(),
            )
            .structure(
                gstreamer::Structure::builder("video/x-raw")
                    .field("format", VideoFormat::Nv12.to_str().to_owned())
                    .build(),
            )
            .build()
    }

    fn ir_depth_caps() -> gstreamer::Caps {
        gstreamer::Caps::builder("video/x-raw")
            .field("format", VideoFormat::Gray16Le.to_str().to_owned())
            .build()
    }
}

#[glib::object_subclass]
impl ObjectSubclass for K4a {
    const NAME: &'static str = "k4asrc";

    type Type = super::K4a;

    type ParentType = gstreamer_base::PushSrc;

    fn new() -> Self {
        Self {
            settings: RwLock::new(Settings::default()),
            state: RwLock::new(State::default()),
            frame_data: Arc::new(FrameData::default()),
        }
    }
}

impl ObjectImpl for K4a {
    fn properties() -> &'static [glib::ParamSpec] {
        static PROPERTIES: Lazy<Vec<glib::ParamSpec>> = Lazy::new(|| {
            SettingField::iter()
                .map(|setting| match setting {
                    SettingField::Fps => {
                        glib::ParamSpecEnum::builder(setting.into(), Settings::default().fps_mode)
                            .nick("FPS")
                            .blurb("The FPS to read from the camera")
                            .build()
                    }
                    SettingField::ColorResolution => {
                        glib::ParamSpecEnum::builder(setting.into(), Settings::default().color_resolution)
                            .nick("Color Resolution")
                            .blurb("The color resolution to read from the camera")
                            .build()
                    }
                    SettingField::DepthMode => {
                        glib::ParamSpecEnum::builder(setting.into(), Settings::default().depth_mode)
                            .nick("Depth Mode")
                            .blurb("The depth mode to read from the camera")
                            .build()
                    }
                    SettingField::Mode => {
                        glib::ParamSpecEnum::builder(setting.into(), Settings::default().mode)
                            .nick("Mode")
                            .blurb("What image type to read from the camera")
                            .build()
                    }
                })
                .collect()
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
                    SettingField::Fps => {
                        set_field!(CAT, self, field, enum settings.fps_mode, value);
                    }
                    SettingField::ColorResolution => {
                        set_field!(CAT, self, field, enum settings.color_resolution, value)
                    }
                    SettingField::DepthMode => {
                        set_field!(CAT, self, field, enum settings.depth_mode, value)
                    }
                    SettingField::Mode => set_field!(CAT, self, field, enum settings.mode, value),
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
                    SettingField::Fps => settings.fps_mode.to_value(),
                    SettingField::ColorResolution => settings.color_resolution.to_value(),
                    SettingField::DepthMode => settings.depth_mode.to_value(),
                    SettingField::Mode => settings.mode.to_value(),
                }
            }
            Err(_err) => {
                panic!("Unknown field {}", pspec.name())
            }
        }
    }
}

impl GstObjectImpl for K4a {}

impl ElementImpl for K4a {
    fn metadata() -> Option<&'static gstreamer::subclass::ElementMetadata> {
        static ELEMENT_METADATA: Lazy<gstreamer::subclass::ElementMetadata> = Lazy::new(|| {
            gstreamer::subclass::ElementMetadata::new(
                "Azure Kinect Source",
                "Source/Video",
                "Azure Kinect Source",
                "William Zhang <wtzhang23@gmail.com>",
            )
        });

        Some(&*ELEMENT_METADATA)
    }

    fn pad_templates() -> &'static [gstreamer::PadTemplate] {
        static PAD_TEMPLATES: Lazy<Vec<gstreamer::PadTemplate>> = Lazy::new(|| {
            [K4a::ir_depth_caps(), K4a::color_caps()]
                .into_iter()
                .map(|caps| {
                    gstreamer::PadTemplate::new(
                        "src",
                        gstreamer::PadDirection::Src,
                        gstreamer::PadPresence::Always,
                        &caps,
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
                let mut state = self.state.write().unwrap();
                let camera = Device::new().map_err(|err| {
                    gstreamer::element_imp_error!(
                        self,
                        gstreamer::LibraryError::Init,
                        ("Could not fetch k4a device. Error: {:#?}", err)
                    );
                    gstreamer::StateChangeError
                })?;
                state.camera.replace(StreamState::Closed(camera));
            }
            gstreamer::StateChange::ReadyToNull => {
                let mut state = self.state.write().unwrap();
                state.camera.take();
            }
            _ => (),
        }
        self.parent_change_state(transition)
    }
}

impl BaseSrcImpl for K4a {
    fn negotiate(&self) -> Result<(), gstreamer::LoggableError> {
        let state = self.state.read().unwrap();
        if let Some(caps) = state.cap_to_use.as_ref() {
            self.instance()
                .set_caps(caps)
                .map_err(|_| gstreamer::loggable_error!(CAT, "Failed to negotiate caps",))
        } else {
            Err(gstreamer::loggable_error!(CAT, "Failed to negotiate caps",))
        }
    }

    fn caps(&self, filter: Option<&gstreamer::Caps>) -> Option<gstreamer::Caps> {
        let mode = self.settings.read().unwrap().mode;
        let to_match = match mode {
            Mode::Color => Self::color_caps(),
            Mode::Ir | Mode::Depth => Self::ir_depth_caps(),
        };
        if let Some(filter) = filter {
            if filter.can_intersect(&to_match) {
                Some(to_match.intersect(filter))
            } else {
                None
            }
        } else {
            Some(to_match)
        }
    }

    fn start(&self) -> Result<(), gstreamer::ErrorMessage> {
        let settings = self.settings.read().unwrap().clone();
        let mut state = self.state.write().unwrap();
        let Some(format) = state.cap_to_use.as_ref().and_then(|caps| caps.structure(0)).and_then(|structure| structure.get::<String>("format").ok()).map(
            |format| VideoFormat::from_string(&format)
        ) else {
            return Err(gstreamer::error_msg!(
                gstreamer::CoreError::Negotiation,
                ("Unknown caps.")
            ));
        };
        gstreamer::info!(CAT, imp: self, "Starting camera stream",);
        let (color_format, color_resolution, depth_mode) = match settings.mode {
            Mode::Color => {
                let format = match format {
                    VideoFormat::Bgra => {
                        libk4a::sys::k4a_image_format_t::K4A_IMAGE_FORMAT_COLOR_BGRA32
                    }
                    VideoFormat::Yuy2 => {
                        libk4a::sys::k4a_image_format_t::K4A_IMAGE_FORMAT_COLOR_YUY2
                    }
                    VideoFormat::Nv12 => {
                        libk4a::sys::k4a_image_format_t::K4A_IMAGE_FORMAT_COLOR_NV12
                    }
                    _ => unreachable!(),
                };
                let resolution = match settings.color_resolution {
                    ColorResolution::Res720P => {
                        libk4a::sys::k4a_color_resolution_t::K4A_COLOR_RESOLUTION_720P
                    }
                    ColorResolution::Res1080P => {
                        libk4a::sys::k4a_color_resolution_t::K4A_COLOR_RESOLUTION_1080P
                    }
                    ColorResolution::Res1440P => {
                        libk4a::sys::k4a_color_resolution_t::K4A_COLOR_RESOLUTION_1440P
                    }
                    ColorResolution::Res1536P => {
                        libk4a::sys::k4a_color_resolution_t::K4A_COLOR_RESOLUTION_1536P
                    }
                    ColorResolution::Res2160P => {
                        libk4a::sys::k4a_color_resolution_t::K4A_COLOR_RESOLUTION_2160P
                    }
                    ColorResolution::Res3072P => {
                        libk4a::sys::k4a_color_resolution_t::K4A_COLOR_RESOLUTION_3072P
                    }
                };
                let depth_mode = libk4a::sys::k4a_depth_mode_t::K4A_DEPTH_MODE_OFF;
                (format, resolution, depth_mode)
            }
            Mode::Ir => {
                let format = libk4a::sys::k4a_image_format_t::K4A_IMAGE_FORMAT_COLOR_MJPG; // default value for disabled
                let resolution = libk4a::sys::k4a_color_resolution_t::K4A_COLOR_RESOLUTION_OFF;
                let depth_mode = libk4a::sys::k4a_depth_mode_t::K4A_DEPTH_MODE_PASSIVE_IR;
                (format, resolution, depth_mode)
            }
            Mode::Depth => {
                let format = libk4a::sys::k4a_image_format_t::K4A_IMAGE_FORMAT_COLOR_MJPG; // default value for disabled
                let resolution = libk4a::sys::k4a_color_resolution_t::K4A_COLOR_RESOLUTION_OFF;
                let depth_mode = match settings.depth_mode {
                    DepthMode::NormalFov2x2Binned => {
                        libk4a::sys::k4a_depth_mode_t::K4A_DEPTH_MODE_NFOV_2X2BINNED
                    }
                    DepthMode::NormalFovUnbinned => {
                        libk4a::sys::k4a_depth_mode_t::K4A_DEPTH_MODE_NFOV_UNBINNED
                    }
                    DepthMode::WideFov2x2Binned => {
                        libk4a::sys::k4a_depth_mode_t::K4A_DEPTH_MODE_WFOV_2X2BINNED
                    }
                    DepthMode::WideFovUnbinned => {
                        libk4a::sys::k4a_depth_mode_t::K4A_DEPTH_MODE_NFOV_UNBINNED
                    }
                };
                (format, resolution, depth_mode)
            }
        };
        let camera_fps = match settings.fps_mode {
            FpsMode::Fps5 => libk4a::sys::k4a_fps_t::K4A_FRAMES_PER_SECOND_5,
            FpsMode::Fps15 => libk4a::sys::k4a_fps_t::K4A_FRAMES_PER_SECOND_15,
            FpsMode::Fps30 => libk4a::sys::k4a_fps_t::K4A_FRAMES_PER_SECOND_30,
        };
        let stream_state = state.camera.take();
        match stream_state {
            Some(StreamState::Closed(camera)) => {
                state.camera = Some(StreamState::Open(
                    camera
                        .start_cameras(libk4a::sys::k4a_device_configuration_t {
                            color_format,
                            color_resolution,
                            depth_mode,
                            camera_fps,
                            synchronized_images_only: Default::default(),
                            depth_delay_off_color_usec: Default::default(),
                            wired_sync_mode:
                                libk4a::sys::k4a_wired_sync_mode_t::K4A_WIRED_SYNC_MODE_STANDALONE,
                            subordinate_delay_off_master_usec: Default::default(),
                            disable_streaming_indicator: Default::default(),
                        })
                        .map_err(|err| {
                            gstreamer::error_msg!(
                                gstreamer::CoreError::Failed,
                                ("Cannot open device to begin streaming. Error: {:#?}", err)
                            )
                        })?,
                ))
            }
            stream_state => {
                state.camera = stream_state;
                return Err(gstreamer::error_msg!(
                    gstreamer::LibraryError::Init,
                    ("Camera not initialized and ready to start streaming.")
                ));
            }
        };
        Ok(())
    }

    fn stop(&self) -> Result<(), gstreamer::ErrorMessage> {
        let mut state = self.state.write().unwrap();
        match state.camera.take() {
            Some(StreamState::Open(stream)) => {
                state.camera = Some(StreamState::Closed(stream.stop_cameras()));
            }
            stream_state => {
                state.camera = stream_state;
            }
        }
        Ok(())
    }

    fn query(&self, query: &mut gstreamer::QueryRef) -> bool {
        match query.view_mut() {
            gstreamer::QueryViewMut::Latency(latency) => {
                latency.set(
                    true,
                    gstreamer::ClockTime::from_nseconds(
                        Duration::from_secs_f64(
                            1001f64 / (self.settings.read().unwrap().fps_mode.fps() * 1000) as f64,
                        )
                        .as_nanos() as u64,
                    ),
                    None,
                );
                true
            }
            gstreamer::QueryViewMut::Caps(caps_query) => {
                if let Some(Ok(format)) = self.caps(
                    caps_query.filter().map(|cap| cap.to_owned()).as_ref()
                ).and_then(|caps| {
                    caps
                        .structure(0)
                        .map(|structure_ref| structure_ref.get::<String>("format"))
                }) {
                    let format: VideoFormat = VideoFormat::from_string(
                        &format,
                    );
                    let (width, height, fps) = {
                        let settings = self.settings.read().unwrap();
                        let (width, height) = settings.resolution();
                        let fps = settings.fps_mode.fps();
                        (width, height, fps)
                    };
                    let caps = Caps::builder("video/x-raw")
                        .field("format", format.to_str().to_owned())
                        .field("width", width)
                        .field("height", height)
                        .field("framerate", gstreamer::Fraction::new(fps * 1000, 1001))
                        .build();
                    self.state.write().unwrap().cap_to_use.replace(caps);
                    caps_query.set_result(self.state.read().unwrap().cap_to_use.clone().as_ref());
                } else {
                    caps_query.set_result(None);
                }
                true
            }
            _ => BaseSrcImplExt::parent_query(self, query),
        }
    }
}

impl PushSrcImpl for K4a {
    fn create(
        &self,
        _buffer: Option<&mut gstreamer::BufferRef>,
    ) -> Result<CreateSuccess, gstreamer::FlowError> {

        let (mode, fps) = {
            let settings = self.settings.read().unwrap();
            (settings.mode, settings.fps_mode.fps())
        };
        let image_type = match mode {
            Mode::Color => libk4a::ImageType::Color,
            Mode::Ir => libk4a::ImageType::Infrared,
            Mode::Depth => libk4a::ImageType::Depth,
        };
        let mut state = self.state.write().unwrap();
        let Some(StreamState::Open(stream)) = state.camera.as_mut() else {
            return Err(gstreamer::FlowError::NotLinked);
        };
        let capture = stream.get_capture().map_err(|err| {
            gstreamer::element_imp_error!(
                self,
                gstreamer::CoreError::Failed,
                ("Could not capture from device. Error: {:#?}", err)
            );
            gstreamer::FlowError::Error
        })?;

        let Some(image) = capture.get_image(image_type) else {
            gstreamer::element_imp_error!(
                self,
                gstreamer::CoreError::Failed,
                ("Could not get image from capture.")
            );
            return Err(gstreamer::FlowError::Error);
        };

        let Some(image_bytes) = image.buffer() else {
            gstreamer::element_imp_error!(
                self,
                gstreamer::CoreError::Failed,
                ("Could not get raw pixels from image.")
            );
            return Err(gstreamer::FlowError::Error);
        };

        let mut buffer = gstreamer::Buffer::with_size(image_bytes.len()).map_err(|err| {
            gstreamer::element_imp_error!(
                self,
                gstreamer::CoreError::Failed,
                ("Could not allocate buffer. Error: {:#?}", err)
            );
            gstreamer::FlowError::Error
        })?;

        buffer
            .make_mut()
            .map_writable()
            .map_err(|err| {
                gstreamer::element_imp_error!(
                    self,
                    gstreamer::CoreError::Failed,
                    ("Could not make buffer writable. Error: {:#?}", err)
                );
                gstreamer::FlowError::Error
            })?
            .copy_from_slice(image_bytes);

        let start_timestamp = self
            .frame_data
            .start_timestamp(image.get_system_timestamp());

        let pts = image.get_system_timestamp() - start_timestamp;
        let duration = Duration::from_secs_f64(1001f64 / (fps * 1000) as f64);

        buffer
            .make_mut()
            .set_pts(gstreamer::ClockTime::from_nseconds(pts.as_nanos() as u64));
        buffer.make_mut().set_dts(None);
        buffer
            .make_mut()
            .set_offset(self.frame_data.add_frame(()) as u64);
        buffer
            .make_mut()
            .set_duration(gstreamer::ClockTime::from_nseconds(
                duration.as_nanos() as u64
            ));

        gstreamer::debug!(
            CAT,
            imp: self,
            "Got frame from camera. pts={:?}, dts={:?}, duration={:?}",
            buffer.as_ref().pts(),
            buffer.as_ref().dts(),
            buffer.as_ref().duration()
        );

        Ok(CreateSuccess::NewBuffer(buffer))
    }
}

#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Clone, Copy, glib::Enum)]
#[repr(u32)]
#[enum_type(name = "GstK4aFpsMode")]
enum FpsMode {
    Fps5,
    Fps15,
    Fps30,
}

impl FpsMode {
    fn fps(&self) -> i32 {
        match self {
            FpsMode::Fps5 => 5,
            FpsMode::Fps15 => 15,
            FpsMode::Fps30 => 30,
        }
    }
}

#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Clone, Copy, glib::Enum)]
#[repr(u32)]
#[enum_type(name = "GstK4aColorResolution")]
enum ColorResolution {
    Res720P,
    Res1080P,
    Res1440P,
    Res1536P,
    Res2160P,
    Res3072P,
}

impl ColorResolution {
    fn dimensions(&self) -> (i32, i32) {
        match self {
            Self::Res720P => (1280, 720),
            Self::Res1080P => (1920, 1080),
            Self::Res1440P => (2560, 1440),
            Self::Res1536P => (2048, 1536),
            Self::Res2160P => (3840, 2160),
            Self::Res3072P => (4096, 3072),
        }
    }
}

#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Clone, Copy, glib::Enum)]
#[repr(u32)]
#[enum_type(name = "GstK4aDepthMode")]
enum DepthMode {
    NormalFov2x2Binned,
    NormalFovUnbinned,
    WideFov2x2Binned,
    WideFovUnbinned,
}

impl DepthMode {
    fn dimensions(&self) -> (i32, i32) {
        match self {
            Self::NormalFov2x2Binned => (320, 288),
            Self::NormalFovUnbinned => (640, 576),
            Self::WideFov2x2Binned => (512, 512),
            Self::WideFovUnbinned => (1024, 1024),
        }
    }
}

#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Clone, Copy, glib::Enum)]
#[repr(u32)]
#[enum_type(name = "GstK4aColorFormat")]
enum ColorFormat {
    Nv12,
    Yuy2,
    Bgra32,
    Depth16,
    Ir16,
}

#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Clone, Copy, glib::Enum)]
#[repr(u32)]
#[enum_type(name = "GstK4aMode")]
enum Mode {
    Color,
    Ir,
    Depth,
}
