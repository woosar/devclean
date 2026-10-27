use std::path::PathBuf;

/// Messages streamed from the cleaner thread to the UI.
pub enum CleanEvent {
    Working(PathBuf),
    Removed { path: PathBuf, bytes: u64 },
    Failed { path: PathBuf, bytes: u64, error: String },
    Finished { cancelled: bool },
}
