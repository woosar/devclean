use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use crate::data_structures::{Language, Relic};

/// Aggregated results of scanning one directory tree.
pub struct ScanReport {
    pub root: PathBuf,
    pub projects: Vec<(PathBuf, Vec<Language>)>,
    /// Sorted by size, largest first.
    pub relics: Vec<Relic>,
    pub dirs_scanned: u64,
    pub cancelled: bool,
    started: Instant,
    elapsed: Option<Duration>,
}

impl ScanReport {
    pub const KINDS: [&'static str; 6] = ["target", "bin", "obj", "venv", "node_modules", "cache"];

    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            projects: Vec::new(),
            relics: Vec::new(),
            dirs_scanned: 0,
            cancelled: false,
            started: Instant::now(),
            elapsed: None,
        }
    }

    pub fn insert_relic(&mut self, relic: Relic) {
        let pos = self.relics.partition_point(|r| r.bytes >= relic.bytes);
        self.relics.insert(pos, relic);
    }

    pub fn finish(&mut self, cancelled: bool) {
        self.cancelled = cancelled;
        self.elapsed = Some(self.started.elapsed());
    }

    pub fn running(&self) -> bool {
        self.elapsed.is_none()
    }

    pub fn elapsed(&self) -> Duration {
        self.elapsed.unwrap_or_else(|| self.started.elapsed())
    }

    /// Projects with a manifest, plus directories that own a relic (e.g. a venv next to loose scripts).
    pub fn project_count(&self, language: Option<Language>) -> usize {
        let matches = |l: Language| language.is_none_or(|want| want == l);
        let mut set: HashSet<&Path> = self
            .projects
            .iter()
            .filter(|(_, langs)| langs.iter().any(|&l| matches(l)))
            .map(|(p, _)| p.as_path())
            .collect();
        set.extend(self.relics.iter().filter(|r| matches(r.language)).map(|r| r.project.as_path()));
        set.len()
    }

    pub fn dirty_count(&self, language: Option<Language>) -> usize {
        self.relics
            .iter()
            .filter(|r| language.is_none_or(|l| l == r.language))
            .map(|r| r.project.as_path())
            .collect::<HashSet<_>>()
            .len()
    }

    pub fn bytes(&self, language: Option<Language>) -> u64 {
        self.relics.iter().filter(|r| language.is_none_or(|l| l == r.language)).map(|r| r.bytes).sum()
    }

    pub fn files(&self) -> u64 {
        self.relics.iter().map(|r| r.files).sum()
    }

    pub fn selected_bytes(&self) -> u64 {
        self.relics.iter().filter(|r| r.selected).map(|r| r.bytes).sum()
    }

    pub fn selected_count(&self) -> usize {
        self.relics.iter().filter(|r| r.selected).count()
    }

    pub fn selected_by_language(&self) -> Vec<(Language, usize, u64)> {
        Language::ALL
            .iter()
            .map(|&lang| {
                let picked = self.relics.iter().filter(|r| r.selected && r.language == lang);
                let (count, bytes) = picked.fold((0, 0), |(c, b), r| (c + 1, b + r.bytes));
                (lang, count, bytes)
            })
            .filter(|&(_, count, _)| count > 0)
            .collect()
    }

    pub fn selected_projects(&self) -> usize {
        self.relics.iter().filter(|r| r.selected).map(|r| r.project.as_path()).collect::<HashSet<_>>().len()
    }

    /// `(kind, bytes, count)` for every relic category that occurred.
    pub fn kind_breakdown(&self) -> Vec<(&'static str, u64, usize)> {
        Self::KINDS
            .iter()
            .map(|&kind| {
                let (bytes, count) = self
                    .relics
                    .iter()
                    .filter(|r| r.kind == kind)
                    .fold((0, 0), |(b, c), r| (b + r.bytes, c + 1));
                (kind, bytes, count)
            })
            .filter(|&(_, _, count)| count > 0)
            .collect()
    }

    pub fn toggle(&mut self, index: usize) {
        if let Some(relic) = self.relics.get_mut(index) {
            relic.selected = !relic.selected;
        }
    }

    /// Select everything, or nothing if everything already is selected.
    pub fn toggle_all(&mut self) {
        let target = self.relics.iter().any(|r| !r.selected);
        self.relics.iter_mut().for_each(|r| r.selected = target);
    }

    pub fn remove_relic(&mut self, path: &Path) {
        self.relics.retain(|r| r.path != path);
    }

    pub fn mark_failed(&mut self, path: &Path, error: String) {
        if let Some(relic) = self.relics.iter_mut().find(|r| r.path == path) {
            relic.selected = false;
            relic.error = Some(error);
        }
    }
}
