use std::{
    sync::{atomic::AtomicUsize, Condvar, Mutex},
    time::Duration,
};

use crossbeam::{atomic::AtomicCell, queue::ArrayQueue};

pub struct FrameData<T> {
    buffer: ArrayQueue<T>,
    start_timestamp: AtomicCell<Option<Duration>>,
    latency: AtomicCell<Option<f64>>,
    park: (Mutex<()>, Condvar),
    frame_counter: AtomicUsize,
}

impl<T> FrameData<T> {
    const BUFFER_SZ: usize = 1;
    const EXP_MOVING_AVG_COEF: f64 = 0.8f64;

    #[allow(unused)]
    pub fn start_timestamp(&self, timestamp: Duration) -> Duration {
        match self.start_timestamp.compare_exchange(None, Some(timestamp)) {
            Ok(None) => timestamp,
            Err(Some(prev)) => prev,
            _ => unreachable!(),
        }
    }

    pub fn update_latency(&self, latency: Duration) {
        if let Some(past_sample) = self.latency.load() {
            self.latency.store(Some(
                past_sample * (1.0f64 - Self::EXP_MOVING_AVG_COEF)
                    + latency.as_nanos() as f64 * Self::EXP_MOVING_AVG_COEF,
            ))
        } else {
            self.latency.store(Some(latency.as_nanos() as f64))
        }
    }

    pub fn get_latency(&self) -> Option<Duration> {
        self.latency
            .load()
            .map(|timestamp| Duration::from_nanos(timestamp as u64))
    }

    pub fn add_frame(&self, frame: T) -> usize {
        self.buffer.force_push(frame);
        let (_lock, cv) = &self.park;
        cv.notify_one();
        self.frame_counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    pub fn pop_frame(&self) -> Option<T> {
        self.buffer.pop()
    }

    pub fn wait(&self) {
        let (lock, cv) = &self.park;
        std::mem::drop(cv.wait(lock.lock().unwrap()).unwrap());
    }
}

impl<T> Default for FrameData<T> {
    fn default() -> Self {
        Self {
            buffer: ArrayQueue::new(Self::BUFFER_SZ),
            start_timestamp: AtomicCell::new(None),
            park: (Mutex::new(()), Condvar::new()),
            latency: AtomicCell::new(None),
            frame_counter: AtomicUsize::new(0),
        }
    }
}

impl<T> Drop for FrameData<T> {
    fn drop(&mut self) {
        let (_lock, cv) = &self.park;
        cv.notify_all();
    }
}
