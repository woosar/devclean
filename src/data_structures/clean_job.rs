use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool, mpsc::Receiver},
    time::Instant,
};

use crate::data_structures::CleanEvent;

/// A deletion run on a background thread.
pub struct CleanJob {
    pub rx: Receiver<CleanEvent>,
    pub cancel: Arc<AtomicBool>,
    pub total: usize,
    pub total_bytes: u64,
    pub done: usize,
    pub done_bytes: u64,
    pub freed: u64,
    pub failed: usize,
    pub current: PathBuf,
    pub started: Instant,
}

impl CleanJob {
    pub fn new(rx: Receiver<CleanEvent>, cancel: Arc<AtomicBool>, total: usize, total_bytes: u64) -> Self {
        Self {
            rx,
            cancel,
            total,
            total_bytes,
            done: 0,
            done_bytes: 0,
            freed: 0,
            failed: 0,
            current: PathBuf::new(),
            started: Instant::now(),
        }
    }

    pub fn ratio(&self) -> f64 {
        if self.total_bytes == 0 {
            return if self.total == 0 { 1.0 } else { self.done as f64 / self.total as f64 };
        }
        (self.done_bytes as f64 / self.total_bytes as f64).clamp(0.0, 1.0)
    }
}
