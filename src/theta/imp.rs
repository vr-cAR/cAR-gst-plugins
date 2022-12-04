use std::sync::{Mutex, Condvar, Weak};
use std::{
    str::FromStr,
    sync::Arc,
};

use gstreamer::subclass::prelude::*;
use gstreamer::{glib, prelude::*};
use gstreamer_base::subclass::prelude::*;
use gstreamer_base::{prelude::*, subclass::base_src::CreateSuccess};
use once_cell::sync::Lazy;
use strum_macros::{EnumString, IntoStaticStr};

use crate::libuvc_theta::{UvcContext, UvcDevice, UvcFrame, UvcStreamHandle};

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
        let mode = Mode::Uhd;
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

#[derive(Clone, Copy, Debug, PartialEq, EnumString, IntoStaticStr)]
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
    frame: Option<gstreamer::Buffer>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            device: None,
            stream: None,
            frame: None,
        }
    }
}

pub struct ThetaUvc {
    settings: Mutex<Settings>,
    state: Arc<Mutex<State>>,
    cv: Condvar,
}

#[glib::object_subclass]
impl ObjectSubclass for ThetaUvc {
    const NAME: &'static str = "thetauvcsrc";

    type Type = super::ThetaUvc;

    type ParentType = gstreamer_base::PushSrc;

    fn new() -> Self {
        Self {
            settings: Mutex::new(Settings::default()),
            state: Arc::new(Mutex::new(State::default())),
            cv: Condvar::new(),
        }
    }
}

impl ObjectImpl for ThetaUvc {
    fn properties() -> &'static [glib::ParamSpec] {
        static PROPERTIES: Lazy<Vec<glib::ParamSpec>> = Lazy::new(|| {
            vec![
                glib::ParamSpecUInt::builder(SettingField::Width.into())
                    .nick("Camera Width")
                    .blurb("The width of the camera stream")
                    .build(),
                glib::ParamSpecUInt::builder(SettingField::Height.into())
                    .nick("Camera Height")
                    .blurb("The height of the camera stream")
                    .build(),
                glib::ParamSpecUInt::builder(SettingField::Fps.into())
                    .nick("Camera FPS")
                    .blurb("The FPS to read from the camera")
                    .build(),
                glib::ParamSpecEnum::builder(SettingField::Mode.into(), Mode::NoMode)
                    .nick("Stream Mode Presets")
                    .blurb("Which preset to use for streaming")
                    .build(),
                glib::ParamSpecEnum::builder(SettingField::Product.into(), Product::AnyProduct)
                    .nick("Ricoh Theta Product")
                    .blurb("The product type of the camera")
                    .build(),
                glib::ParamSpecString::builder(SettingField::SerialNumber.into())
                    .nick("Device Serial Number")
                    .blurb("The serial number of the device")
                    .build(),
                glib::ParamSpecUInt::builder(SettingField::DeviceIndex.into())
                    .nick("Device Index")
                    .blurb("Given a list of devices that matches the capabilities of the desired device, chooses which one to use")
                    .build(),
                
            ]
        });
        PROPERTIES.as_ref()
    }

    fn constructed(&self) {
        self.parent_constructed();
        let obj = self.instance();
        obj.set_live(true);
        obj.set_format(gstreamer::Format::Time);
    }

    fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
        match SettingField::from_str(pspec.name()) {
            Ok(field) => {
                let mut settings = self.settings.lock().unwrap();
                macro_rules! set_field {
                    ($field:expr, $val: expr) => {
                        {
                            gstreamer::debug!(
                                CAT,
                                imp: self,
                                "Changing {} from {} to {}",
                                Into::<&str>::into(field),
                                $field,
                                $val,
                            );
                            $field = $val;
                        }
                    };
                    ($field:expr) => {
                        {
                            let Ok(new_value) = value.get() else {
                                panic!("Could not deserialize value passed in for {}", Into::<&str>::into(field));
                            };
                            set_field!($field, new_value);
                        }
                    };
                    (enum $field:expr, $val:expr) => {
                        {
                            gstreamer::debug!(
                                CAT,
                                imp: self,
                                "Changing {} from {} to {}",
                                Into::<&str>::into(field),
                                glib::EnumValue::from_value(&$field.to_value()).unwrap().1.name(),
                                glib::EnumValue::from_value(&value).unwrap().1.name(),
                            );
                            $field = $val;
                        }
                    };
                    (enum $field:expr) => {
                        {
                            let Ok(new_value) = value.get() else {
                                panic!("Could not deserialize value passed in for {}", Into::<&str>::into(field));
                            };
                            set_field!(enum $field, new_value);
                        }
                    }
                }
                Mode::Fhd.to_value();
                match field {
                    SettingField::Width => {
                        set_field!(settings.width);
                        set_field!(enum settings.mode, Mode::NoMode);
                    },
                    SettingField::Height => {
                        set_field!(settings.height);
                        set_field!(enum settings.mode, Mode::NoMode);
                    },
                    SettingField::Fps => {
                        set_field!(settings.fps);
                        set_field!(enum settings.mode, Mode::NoMode);
                    },
                    SettingField::Mode => {
                        set_field!(enum settings.mode);
                        if let Some(preset) = settings.mode.get_mode_settings() {
                            set_field!(settings.width, preset.width);
                            set_field!(settings.height, preset.height);
                            set_field!(settings.fps, preset.fps);
                        }
                    },
                    SettingField::Product => set_field!(enum settings.product),
                    SettingField::DeviceIndex => set_field!(settings.device_index),
                    SettingField::SerialNumber => set_field!(settings.serial_number),
                };
            }
            Err(_err) => {
                panic!("Unknown field {}", pspec.name());
            }
        }
    }

    fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
        match SettingField::from_str(pspec.name()) {
            Ok(field) => {
                let settings = self.settings.lock().unwrap();
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
                &gstreamer::Caps::builder("video/x-h264").build(),
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
                    let settings = self.settings.lock().unwrap();
                    settings.clone()
                };
                let context = UvcContext::new().map_err(|err| {
                    gstreamer::element_imp_error!(
                        self,
                        gstreamer::LibraryError::Init,
                        ("{:#?}", err)
                    );
                    gstreamer::StateChangeError
                })?;

                let vid = Some(USBVID_RICOH).map(|i| i as i32);
                let pid = match settings.product {
                    Product::Z1 => Some(USBPID_THETAZ1_UVC),
                    Product::V => Some(USBPID_THETAV_UVC),
                    Product::AnyProduct => None,
                }.map(|i| i as i32);
                let serial_number = match settings.serial_number.as_str() {
                    "" => None,
                    sn => Some(sn),
                };
                let device = context.find_devices(vid, pid, serial_number)
                    .map_err(|err| {
                        gstreamer::element_imp_error!(
                            self,
                            gstreamer::LibraryError::Init,
                            ("Could not find device {}. Error: {:#?}", 
                                [
                                    ("vid", vid.map(|vid| vid.to_string())), 
                                    ("pid", pid.map(|pid| pid.to_string())), 
                                    ("serial number", serial_number.map(|sn| sn.to_owned()))
                                ]
                                .into_iter()
                                .map(|(name, val)| format!("{} = {}", name, match val {
                                    Some(val) => val,
                                    None => "<any>".to_owned()
                                }))
                                .collect::<Vec<_>>()
                                .join(",")
                            , err)
                        );
                        gstreamer::StateChangeError
                    })?;
                if let Some(device) = device.into_iter().nth(settings.device_index as usize) {
                    let mut state = self.state.lock().unwrap();
                    state.device.replace(Arc::new(device));
                } else {
                    gstreamer::element_imp_error!(
                        self,
                        gstreamer::LibraryError::Init,
                        ("Could not find a compatible device")
                    );
                    return Err(gstreamer::StateChangeError);
                }
            }
            gstreamer::StateChange::ReadyToNull => {
                let mut state = self.state.lock().unwrap();
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
        let caps = gstreamer::Caps::builder("video/x-h264").build();
        if let Some(filter) = filter {
            if filter.can_intersect(&caps) {
                Some(caps)
            } else {
                None
            }
        } else {
            Some(caps)
        }
    }

    fn start(&self) -> Result<(), gstreamer::ErrorMessage> {
        let settings = {
            let settings = self.settings.lock().unwrap();
            settings.clone()
        };
        
        let mut state = self.state.lock().unwrap();
        let device = state.device.as_ref().ok_or_else(|| {
            gstreamer::error_msg!(
                gstreamer::LibraryError::Init,
                ["device not initialized yet"]
            )
        })?;
        let device_handle = device
            .open()
            .map_err(|err| gstreamer::error_msg!(gstreamer::LibraryError::Init, ("{:#?}", err)))?;
        let stream_handle = device_handle.start_streaming(
            settings.width as usize,
            settings.height as usize,
            settings.fps as usize,
            on_frame_callback,
            Arc::downgrade(&self.state.clone()),
        )
        .map_err(|err| gstreamer::error_msg!(gstreamer::LibraryError::Init, ("{:#?}", err)))?;
        state.stream.replace(stream_handle);

        Ok(())
    }

    fn stop(&self) -> Result<(), gstreamer::ErrorMessage> {
        let mut state = self.state.lock().unwrap();
        state.device.take();
        state.frame.take();
        Ok(())
    }
}

impl PushSrcImpl for ThetaUvc {
    fn create(
        &self,
        _buffer: Option<&mut gstreamer::BufferRef>,
    ) -> Result<CreateSuccess, gstreamer::FlowError> {
        let mut state = self.state.lock().unwrap();
        loop {
            if let Some(buffer) = state.frame.take() {
                return Ok(CreateSuccess::NewBuffer(buffer))
            }
            state = self.cv.wait(state).unwrap();
        }
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

fn on_frame_callback(frame: UvcFrame, state: &mut Weak<Mutex<State>>) {
    let Some(state) = state.upgrade() else {
        return;
    };
    let mut state = state.lock().unwrap();
    
    let mut buffer = gstreamer::Buffer::from_mut_slice(frame.data().to_vec());

    let frame_interval = state.stream.as_ref().unwrap().frame_interval().as_nanos() as u64;
    let pts = gstreamer::ClockTime::from_useconds(frame_interval * frame.sequence() as u64);
    buffer.make_mut().set_pts(Some(pts));
    buffer.make_mut().set_dts(None);
    buffer.make_mut().set_duration(Some(gstreamer::ClockTime::from_useconds(frame_interval)));
    state.frame.replace(buffer);
}

#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Clone, Copy, glib::Enum)]
#[repr(u32)]
#[enum_type(name = "GstThetaUvcProduct")]
pub enum Product {
    Z1,
    V,
    AnyProduct,
}