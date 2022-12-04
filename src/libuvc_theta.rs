use std::{
    ffi::CString,
    mem::MaybeUninit,
    os::raw::c_void,
    ptr::NonNull,
    sync::{Arc, Mutex},
    time::Duration,
};

use self::sys::uvc_stream_ctrl_t;

type PossibleStream = Mutex<(Option<Box<dyn Stream>>, Arc<UvcStreamHandleWrapper>)>;

struct UvcContextWrapper {
    ctx: NonNull<sys::uvc_context>,
}

impl UvcContextWrapper {
    unsafe fn new() -> Result<Self, sys::uvc_error_t> {
        let mut ctx = std::ptr::null_mut();
        match sys::uvc_init((&mut ctx) as *mut _, std::ptr::null_mut()) {
            sys::uvc_error_t::UVC_SUCCESS => Ok(Self {
                ctx: NonNull::new(ctx).unwrap(),
            }),
            err => Err(err),
        }
    }

    unsafe fn find_devices(
        self: &Arc<Self>,
        vid: Option<i32>,
        pid: Option<i32>,
        serial_number: Option<&str>,
    ) -> Result<Vec<UvcDevice>, sys::uvc_error_t> {
        let mut device_ptr_arr = MaybeUninit::<*mut *mut sys::uvc_device>::uninit();
        let serial_number = serial_number
            .as_ref()
            .map(|sn| CString::new(sn.as_bytes()).unwrap());

        match sys::uvc_find_devices(
            self.ctx.as_ptr(),
            device_ptr_arr.as_mut_ptr(),
            vid.unwrap_or(0),
            pid.unwrap_or(0),
            serial_number
                .as_ref()
                .map(|sn| sn.as_ptr())
                .unwrap_or(std::ptr::null_mut()),
        ) {
            sys::uvc_error_t::UVC_SUCCESS => {
                let device_ptr_slice = device_ptr_arr.assume_init();
                let mut devices = Vec::new();
                for i in 0.. {
                    let device_ptr = *device_ptr_slice.offset(i).as_ref().unwrap();
                    match NonNull::new(device_ptr) {
                        Some(device_ptr) => devices.push(UvcDevice::new(UvcDeviceWrapper::new(
                            device_ptr,
                            self.clone(),
                        ))),
                        None => {
                            break;
                        }
                    }
                }
                libc::free(device_ptr_slice as *mut c_void);

                Ok(devices)
            }
            err => Err(err),
        }
    }
}

impl Drop for UvcContextWrapper {
    fn drop(&mut self) {
        unsafe { sys::uvc_exit(self.ctx.as_ptr()) }
    }
}

unsafe impl Send for UvcContextWrapper {}
unsafe impl Sync for UvcContextWrapper {}

pub struct UvcContext {
    inner: Arc<UvcContextWrapper>,
}

impl UvcContext {
    pub fn new() -> Result<Self, sys::uvc_error> {
        Ok(Self {
            inner: Arc::new(unsafe { UvcContextWrapper::new()? }),
        })
    }

    pub fn find_devices(
        &self,
        vid: Option<i32>,
        pid: Option<i32>,
        serial_number: Option<&str>,
    ) -> Result<Vec<UvcDevice>, sys::uvc_error> {
        Ok(unsafe { self.inner.find_devices(vid, pid, serial_number)? })
    }
}

struct UvcDeviceWrapper {
    dev: NonNull<sys::uvc_device>,
    _owner: Arc<UvcContextWrapper>,
}

impl UvcDeviceWrapper {
    unsafe fn new(dev: NonNull<sys::uvc_device>, ctx: Arc<UvcContextWrapper>) -> Self {
        Self { dev, _owner: ctx }
    }

    pub unsafe fn open(self: Arc<Self>) -> Result<UvcDeviceHandle, sys::uvc_error> {
        Ok(UvcDeviceHandle::new(UvcDeviceHandleWrapper::new(self)?))
    }
}

impl Drop for UvcDeviceWrapper {
    fn drop(&mut self) {
        unsafe { sys::uvc_unref_device(self.dev.as_ptr()) }
    }
}

unsafe impl Send for UvcDeviceWrapper {}

unsafe impl Sync for UvcDeviceWrapper {}

pub struct UvcDevice {
    inner: Arc<UvcDeviceWrapper>,
}

impl UvcDevice {
    fn new(wrapper: UvcDeviceWrapper) -> Self {
        Self {
            inner: Arc::new(wrapper),
        }
    }

    pub fn open(&self) -> Result<UvcDeviceHandle, sys::uvc_error> {
        unsafe { self.inner.clone().open() }
    }
}

struct UvcDeviceHandleWrapper {
    handle: NonNull<sys::uvc_device_handle>,
    streams: Mutex<Vec<Box<PossibleStream>>>,
    _owner: Arc<UvcDeviceWrapper>,
}

impl UvcDeviceHandleWrapper {
    unsafe fn new(device: Arc<UvcDeviceWrapper>) -> Result<Self, sys::uvc_error> {
        let mut handle_ptr = MaybeUninit::<*mut sys::uvc_device_handle>::uninit();
        match sys::uvc_open(device.dev.as_ptr(), handle_ptr.as_mut_ptr()) {
            sys::uvc_error::UVC_SUCCESS => Ok(Self {
                handle: NonNull::new(handle_ptr.assume_init()).unwrap(),
                streams: Mutex::new(Vec::new()),
                _owner: device,
            }),
            err => Err(err),
        }
    }

    unsafe fn start_streaming<F, T>(
        self: &Arc<Self>,
        width: i32,
        height: i32,
        fps: i32,
        cb: F,
        init: T,
    ) -> Result<UvcStreamHandle, sys::uvc_error>
    where
        F: FnMut(UvcFrame, &mut T) + Send + Sync + 'static,
        T: Send + Sync + 'static,
    {
        let mut ctrl = uvc_stream_ctrl_t::default();
        match sys::uvc_get_stream_ctrl_format_size(
            self.handle.as_ptr(),
            &mut ctrl as *mut _,
            sys::uvc_frame_format::UVC_FRAME_FORMAT_H264,
            width,
            height,
            fps,
        ) {
            sys::uvc_error::UVC_SUCCESS => {}
            err => return Err(err),
        }
        let (handle, state) = UvcStreamHandleWrapper::new(self.clone(), cb, init, &mut ctrl)?;
        self.streams.lock().unwrap().push(state);
        Ok(UvcStreamHandle::new(handle, ctrl))
    }
}

impl Drop for UvcDeviceHandleWrapper {
    fn drop(&mut self) {
        unsafe {
            sys::uvc_stop_streaming(self.handle.as_ptr());
            sys::uvc_close(self.handle.as_ptr());
        }
    }
}

pub struct UvcDeviceHandle {
    inner: Arc<UvcDeviceHandleWrapper>,
}

impl UvcDeviceHandle {
    fn new(inner: UvcDeviceHandleWrapper) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }

    pub fn start_streaming<F, T>(
        self,
        width: usize,
        height: usize,
        fps: usize,
        cb: F,
        init: T,
    ) -> Result<UvcStreamHandle, sys::uvc_error>
    where
        F: FnMut(UvcFrame, &mut T) + Send + Sync + 'static,
        T: Send + Sync + 'static,
    {
        unsafe {
            self.inner.start_streaming(
                width
                    .try_into()
                    .map_err(|_err| sys::uvc_error::UVC_ERROR_NOT_SUPPORTED)?,
                height
                    .try_into()
                    .map_err(|_err| sys::uvc_error::UVC_ERROR_NOT_SUPPORTED)?,
                fps.try_into()
                    .map_err(|_err| sys::uvc_error::UVC_ERROR_NOT_SUPPORTED)?,
                cb,
                init,
            )
        }
    }
}

struct UvcStreamHandleWrapper {
    _owner: Arc<UvcDeviceHandleWrapper>,
}

impl UvcStreamHandleWrapper {
    unsafe fn new<F, T>(
        handle: Arc<UvcDeviceHandleWrapper>,
        cb: F,
        init: T,
        ctrl: &mut uvc_stream_ctrl_t,
    ) -> Result<(Arc<Self>, Box<PossibleStream>), sys::uvc_error>
    where
        F: FnMut(UvcFrame, &mut T) + Send + Sync + 'static,
        T: Send + Sync + 'static,
    {
        let wrapper = Arc::new(Self {
            _owner: handle,
        });
        let mut state: Box<PossibleStream> = Box::new(Mutex::new((Some(Box::new((cb, init)) as Box<dyn Stream>), wrapper.clone())));

        match sys::uvc_start_streaming(
            wrapper._owner.handle.as_ptr(),
            ctrl as *mut _,
            Some(callback),
            state.as_mut() as &mut PossibleStream as *mut PossibleStream as *mut _,
            0,
        ) {
            sys::uvc_error::UVC_SUCCESS => Ok((
                wrapper,
                state,
            )),
            err => Err(err),
        }
    }
}

unsafe impl Send for UvcStreamHandleWrapper {}
unsafe impl Sync for UvcStreamHandleWrapper {}

pub struct UvcStreamHandle {
    inner: Arc<UvcStreamHandleWrapper>,
    ctrl: uvc_stream_ctrl_t,
}

impl UvcStreamHandle {
    fn new(inner: Arc<UvcStreamHandleWrapper>, ctrl: uvc_stream_ctrl_t) -> Self {
        Self {
            inner,
            ctrl,
        }
    }

    pub fn frame_interval(&self) -> Duration {
        Duration::from_nanos(self.ctrl.dwFrameInterval as u64 * 100)
    }
}

trait Stream: Send + Sync {
    fn handle_frame(&mut self, frame: *mut sys::uvc_frame, handle: Arc<UvcStreamHandleWrapper>);
}

impl<F, T> Stream for (F, T)
where
    F: FnMut(UvcFrame, &mut T) + Send + Sync,
    T: Send + Sync,
{
    fn handle_frame(&mut self, frame: *mut sys::uvc_frame, handle: Arc<UvcStreamHandleWrapper>) {
        let (f, val) = self;
        let frame = UvcFrame::new(NonNull::new(frame).unwrap(), handle);
        f(frame, val);
    }
}

pub struct UvcFrame {
    frame: NonNull<sys::uvc_frame>,
    _owner: Arc<UvcStreamHandleWrapper>,
}

impl UvcFrame {
    fn new(frame: NonNull<sys::uvc_frame>, handle: Arc<UvcStreamHandleWrapper>) -> Self {
        Self { frame, _owner: handle }
    }

    pub fn data(&self) -> &[u8] {
        unsafe {
            std::slice::from_raw_parts(
                self.frame.as_ref().data as *const u8,
                self.frame.as_ref().data_bytes,
            )
        }
    }

    pub fn width(&self) -> usize {
        unsafe { self.frame.as_ref().width as usize }
    }

    pub fn height(&self) -> usize {
        unsafe { self.frame.as_ref().height as usize }
    }

    pub fn step(&self) -> usize {
        unsafe { self.frame.as_ref().step as usize }
    }

    pub fn sequence(&self) -> usize {
        unsafe { self.frame.as_ref().sequence as usize }
    }
}

unsafe impl Send for UvcFrame {}

unsafe impl Sync for UvcFrame {}

unsafe extern "C" fn callback(frame: *mut sys::uvc_frame, user_ptr: *mut c_void) {
    let mut state = (user_ptr as *mut PossibleStream)
        .as_mut()
        .unwrap()
        .lock()
        .unwrap();
    let (state, stream_handle) = &mut *state;
    if let Some(stream) = state.as_mut() {
        stream.handle_frame(frame, stream_handle.clone());
    }
}

mod sys {
    #![allow(non_upper_case_globals)]
    #![allow(non_camel_case_types)]
    #![allow(non_snake_case)]

    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}
