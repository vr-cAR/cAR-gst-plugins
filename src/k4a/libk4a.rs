use std::{mem::MaybeUninit, ptr::NonNull, sync::Arc, time::Duration};

pub use sys::k4a_device_configuration_t as DeviceConfiguration;

struct DeviceWrapper {
    device: NonNull<sys::_k4a_device_t>,
}

impl DeviceWrapper {
    unsafe fn new() -> Result<Self, sys::k4a_result_t> {
        let mut device = MaybeUninit::uninit();

        match sys::k4a_device_open(sys::K4A_DEVICE_DEFAULT, device.as_mut_ptr()) {
            sys::k4a_result_t::K4A_RESULT_SUCCEEDED => Ok(Self {
                device: NonNull::new(device.assume_init()).unwrap(),
            }),
            err => Err(err),
        }
    }

    unsafe fn start_cameras(
        &self,
        mut config: sys::k4a_device_configuration_t,
    ) -> Result<(), sys::k4a_result_t> {
        match sys::k4a_device_start_cameras(self.device.as_ptr(), &mut config as *mut _) {
            sys::k4a_result_t::K4A_RESULT_SUCCEEDED => Ok(()),
            err => Err(err),
        }
    }

    unsafe fn stop_cameras(&self) {
        sys::k4a_device_stop_cameras(self.device.as_ptr());
    }

    unsafe fn get_capture(&self) -> Result<NonNull<sys::_k4a_capture_t>, sys::k4a_wait_result_t> {
        let mut handle = MaybeUninit::uninit();
        match sys::k4a_device_get_capture(
            self.device.as_ptr(),
            handle.as_mut_ptr(),
            sys::K4A_WAIT_INFINITE,
        ) {
            sys::k4a_wait_result_t::K4A_WAIT_RESULT_SUCCEEDED => {
                Ok(NonNull::new(handle.assume_init()).unwrap())
            }
            err => Err(err),
        }
    }
}

impl Drop for DeviceWrapper {
    fn drop(&mut self) {
        unsafe {
            sys::k4a_device_close(self.device.as_ptr());
        }
    }
}

unsafe impl Send for DeviceWrapper {}

unsafe impl Sync for DeviceWrapper {}

pub struct Device {
    inner: Arc<DeviceWrapper>,
}

impl Device {
    pub fn new() -> Result<Self, sys::k4a_result_t> {
        Ok(Self {
            inner: Arc::new(unsafe { DeviceWrapper::new()? }),
        })
    }

    pub fn start_cameras(
        self,
        config: sys::k4a_device_configuration_t,
    ) -> Result<Stream, sys::k4a_result_t> {
        unsafe {
            self.inner.start_cameras(config)?;
        }
        Ok(Stream::new(self))
    }

    fn get_capture(&self) -> Result<Capture, sys::k4a_wait_result_t> {
        Ok(Capture::new(CaptureWrapper::new(
            self.inner.clone(),
            unsafe { self.inner.get_capture()? },
        )))
    }

    fn stop_cameras(&self) {
        unsafe {
            self.inner.stop_cameras();
        }
    }
}

pub struct Stream {
    device: Option<Device>,
}

impl Stream {
    fn new(device: Device) -> Self {
        Self {
            device: Some(device),
        }
    }

    pub fn get_capture(&self) -> Result<Capture, sys::k4a_wait_result_t> {
        self.device.as_ref().unwrap().get_capture()
    }

    pub fn stop_cameras(mut self) -> Device {
        let device = self.device.take().unwrap();
        device.stop_cameras();
        device
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        if let Some(device) = self.device.as_ref() {
            device.stop_cameras();
        }
    }
}

struct CaptureWrapper {
    _owner: Arc<DeviceWrapper>,
    capture: NonNull<sys::_k4a_capture_t>,
}

impl CaptureWrapper {
    fn new(device: Arc<DeviceWrapper>, capture: NonNull<sys::_k4a_capture_t>) -> Self {
        Self {
            _owner: device,
            capture,
        }
    }

    unsafe fn get_image(&self, image_type: ImageType) -> Option<NonNull<sys::_k4a_image_t>> {
        let image = match image_type {
            ImageType::Color => sys::k4a_capture_get_color_image(self.capture.as_ptr()),
            ImageType::Infrared => sys::k4a_capture_get_ir_image(self.capture.as_ptr()),
            ImageType::Depth => sys::k4a_capture_get_depth_image(self.capture.as_ptr()),
        };
        NonNull::new(image)
    }
}

impl Drop for CaptureWrapper {
    fn drop(&mut self) {
        unsafe { sys::k4a_capture_release(self.capture.as_ptr()) }
    }
}

unsafe impl Send for CaptureWrapper {}

unsafe impl Sync for CaptureWrapper {}

pub struct Capture {
    inner: Arc<CaptureWrapper>,
}

impl Capture {
    fn new(inner: CaptureWrapper) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }

    pub fn get_image(&self, image_type: ImageType) -> Option<Image> {
        unsafe { self.inner.get_image(image_type) }
            .map(|image| Image::new(self.inner.clone(), image, image_type))
    }
}

pub struct Image {
    _owner: Arc<CaptureWrapper>,
    image: NonNull<sys::_k4a_image_t>,
    image_type: ImageType,
}

impl Image {
    fn new(
        capture: Arc<CaptureWrapper>,
        image: NonNull<sys::_k4a_image_t>,
        image_type: ImageType,
    ) -> Self {
        Self {
            _owner: capture,
            image,
            image_type,
        }
    }

    pub fn buffer(&self) -> &[u8] {
        unsafe {
            let ptr = NonNull::new(sys::k4a_image_get_buffer(self.image.as_ptr())).unwrap();
            let size = sys::k4a_image_get_size(self.image.as_ptr());
            std::slice::from_raw_parts(ptr.as_ptr(), size)
        }
    }

    pub fn get_system_timestamp(&self) -> Duration {
        unsafe {
            Duration::from_nanos(sys::k4a_image_get_system_timestamp_nsec(
                self.image.as_ptr(),
            ))
        }
    }

    #[allow(unused)]
    pub fn image_type(&self) -> ImageType {
        self.image_type
    }
}

impl Drop for Image {
    fn drop(&mut self) {
        unsafe {
            sys::k4a_image_release(self.image.as_ptr());
        }
    }
}

unsafe impl Send for Image {}

unsafe impl Sync for Image {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ImageType {
    Color,
    Infrared,
    Depth,
}

pub mod sys {
    #![allow(non_upper_case_globals)]
    #![allow(non_camel_case_types)]
    #![allow(non_snake_case)]
    #![allow(dead_code)]

    include!(concat!(env!("OUT_DIR"), "/k4a.rs"));
}
