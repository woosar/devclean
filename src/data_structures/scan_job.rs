use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool, mpsc::Receiver},
};

use crate::data_structures::ScanEvent;

/// A scan running on a background thread.
pub struct ScanJob {
    pub rx: Receiver<ScanEvent>,
    pub cancel: Arc<AtomicBool>,
    pub current: PathBuf,
    /// Open the clean dialog as soon as the scan completes.
    pub then_clean: bool,
}

impl ScanJob {
    pub fn new(rx: Receiver<ScanEvent>, cancel: Arc<AtomicBool>, root: PathBuf, then_clean: bool) -> Self {
        Self { rx, cancel, current: root, then_clean }
    }
}
