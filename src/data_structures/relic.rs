use std::path::PathBuf;

use crate::data_structures::Language;

/// A build-output directory that is safe to delete.
#[derive(Debug, Clone)]
pub struct Relic {
    pub path: PathBuf,
    /// The project directory that owns this relic.
    pub project: PathBuf,
    pub language: Language,
    /// Category: `target`, `bin`, `obj`, `venv`, `node_modules` or `cache`.
    pub kind: &'static str,
    pub bytes: u64,
    pub files: u64,
    pub selected: bool,
    pub error: Option<String>,
}
