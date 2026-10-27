use std::path::PathBuf;

use crate::data_structures::{Language, Relic};

/// Messages streamed from the scanner thread to the UI.
pub enum ScanEvent {
    Progress { dirs: u64, current: PathBuf },
    Project { path: PathBuf, languages: Vec<Language> },
    Relic(Relic),
    Finished { dirs: u64, cancelled: bool },
}
