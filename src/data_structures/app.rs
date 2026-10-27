use std::{
    collections::HashMap,
    io,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, TryRecvError},
    },
    time::{Duration, Instant},
};

use ratatui::{
    DefaultTerminal,
    crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    widgets::TableState,
};

use crate::{
    data_structures::{CleanEvent, CleanJob, Platform, Root, RootKind, ScanEvent, ScanJob, ScanReport, Tree},
    helpers,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Tree,
    Relics,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Confirm,
    GoTo(String),
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Info,
    Good,
    Warn,
}

pub struct App {
    pub platform: Platform,
    pub tree: Tree,
    pub focus: Focus,
    pub mode: Mode,
    pub report: Option<ScanReport>,
    pub scan: Option<ScanJob>,
    pub clean: Option<CleanJob>,
    pub relic_state: TableState,
    pub banner: Option<(String, Tone, Instant)>,
    /// `(total, free)` bytes per root path; `None` when the query failed.
    pub disk: HashMap<PathBuf, Option<(u64, u64)>>,
    pub tick: usize,
    quit: bool,
}

const PAGE: isize = 15;

impl App {
    pub fn new(start: Option<PathBuf>) -> Self {
        let platform = Platform::detect();
        let mut tree = Tree::new(helpers::discover_roots(&platform));
        let start = start
            .map(|p| helpers::resolve_input_path(&p.to_string_lossy()))
            .or_else(|| std::env::current_dir().ok());
        if let Some(path) = start {
            tree.reveal(&path);
        }
        let mut app = Self {
            platform,
            tree,
            focus: Focus::Tree,
            mode: Mode::Normal,
            report: None,
            scan: None,
            clean: None,
            relic_state: TableState::default(),
            banner: None,
            disk: HashMap::new(),
            tick: 0,
            quit: false,
        };
        app.refresh_disk();
        app
    }

    pub fn run(mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        while !self.quit {
            self.pump_scan();
            self.pump_clean();
            terminal.draw(|frame| helpers::draw(frame, &mut self))?;
            if event::poll(Duration::from_millis(80))?
                && let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                self.on_key(key);
            }
            self.tick = self.tick.wrapping_add(1);
        }
        for cancel in self.scan.iter().map(|j| &j.cancel).chain(self.clean.iter().map(|j| &j.cancel)) {
            cancel.store(true, Ordering::Relaxed);
        }
        Ok(())
    }

    fn notify(&mut self, message: impl Into<String>, tone: Tone) {
        self.banner = Some((message.into(), tone, Instant::now()));
    }

    fn refresh_disk(&mut self) {
        let Some(node) = self.tree.selected() else { return };
        let Some(root) = self.tree.volume_for(&node.path) else { return };
        let path = root.path.clone();
        self.disk.entry(path).or_insert_with_key(|p| helpers::disk_space(p));
    }

    // ---------------------------------------------------------------- input

    fn on_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if let Some(job) = &self.clean {
            if key.code == KeyCode::Esc {
                job.cancel.store(true, Ordering::Relaxed);
                self.notify("Stopping after the current relic…", Tone::Warn);
            }
            return;
        }
        match self.mode {
            Mode::Help => self.mode = Mode::Normal,
            Mode::Confirm => match key.code {
                KeyCode::Char('y' | 'Y') | KeyCode::Enter => {
                    self.mode = Mode::Normal;
                    self.start_clean();
                }
                KeyCode::Esc | KeyCode::Char('n' | 'N' | 'q') => self.mode = Mode::Normal,
                _ => {}
            },
            Mode::GoTo(_) => self.on_goto_key(key),
            Mode::Normal => self.on_normal_key(key),
        }
        self.refresh_disk();
    }

    fn on_goto_key(&mut self, key: KeyEvent) {
        let Mode::GoTo(input) = &mut self.mode else { return };
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Backspace => {
                input.pop();
            }
            KeyCode::Tab => {
                if let Some(completed) = helpers::complete_path(input) {
                    *input = completed;
                }
            }
            KeyCode::Char(c) => input.push(c),
            KeyCode::Enter => {
                let path = helpers::resolve_input_path(input);
                self.mode = Mode::Normal;
                self.go_to(path);
            }
            _ => {}
        }
    }

    fn go_to(&mut self, path: PathBuf) {
        if !path.is_dir() {
            self.notify(format!("No such directory: {}", path.display()), Tone::Warn);
            return;
        }
        if !self.tree.reveal(&path) {
            self.tree.pin(Root {
                label: path.display().to_string(),
                path: path.clone(),
                kind: RootKind::Pinned,
                fs_type: None,
                volume: None,
            });
            self.tree.reveal(&path);
            self.notify(format!("Pinned {} as a new root", path.display()), Tone::Info);
        }
        self.focus = Focus::Tree;
    }

    fn on_normal_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Esc => {
                if let Some(job) = &self.scan {
                    job.cancel.store(true, Ordering::Relaxed);
                }
            }
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char('/' | 'g') => {
                let mut prefill = self.tree.selected().map(|n| n.path.display().to_string()).unwrap_or_default();
                if !prefill.ends_with(std::path::MAIN_SEPARATOR) {
                    prefill.push(std::path::MAIN_SEPARATOR);
                }
                self.mode = Mode::GoTo(prefill);
            }
            KeyCode::Char('s') => self.start_scan(false),
            KeyCode::Char('c') => self.request_clean(),
            KeyCode::Char('.') => {
                self.tree.toggle_hidden();
                let state = if self.tree.show_hidden { "shown" } else { "hidden" };
                self.notify(format!("Hidden folders {state}"), Tone::Info);
            }
            KeyCode::Char('r') => {
                self.tree.rebuild();
                self.disk.clear();
            }
            KeyCode::Tab | KeyCode::BackTab => self.toggle_focus(),
            _ => match self.focus {
                Focus::Tree => self.on_tree_key(key),
                Focus::Relics => self.on_relic_key(key),
            },
        }
    }

    fn toggle_focus(&mut self) {
        let has_relics = self.report.as_ref().is_some_and(|r| !r.relics.is_empty());
        self.focus = match self.focus {
            Focus::Tree if has_relics => {
                if self.relic_state.selected().is_none() {
                    self.relic_state.select(Some(0));
                }
                Focus::Relics
            }
            _ => Focus::Tree,
        };
    }

    fn on_tree_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.tree.move_by(-1),
            KeyCode::Down | KeyCode::Char('j') => self.tree.move_by(1),
            KeyCode::PageUp => self.tree.move_by(-PAGE),
            KeyCode::PageDown => self.tree.move_by(PAGE),
            KeyCode::Home => self.tree.select_first(),
            KeyCode::End => self.tree.select_last(),
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter => self.tree.right(),
            KeyCode::Left | KeyCode::Char('h') => self.tree.left(),
            KeyCode::Backspace => self.tree.select_parent(),
            _ => {}
        }
    }

    fn on_relic_key(&mut self, key: KeyEvent) {
        let Some(report) = &mut self.report else { return };
        let len = report.relics.len();
        if len == 0 {
            self.focus = Focus::Tree;
            return;
        }
        let current = self.relic_state.selected().unwrap_or(0).min(len - 1);
        let step = |delta: isize| (current as isize + delta).clamp(0, len as isize - 1) as usize;
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.relic_state.select(Some(step(-1))),
            KeyCode::Down | KeyCode::Char('j') => self.relic_state.select(Some(step(1))),
            KeyCode::PageUp => self.relic_state.select(Some(step(-PAGE))),
            KeyCode::PageDown => self.relic_state.select(Some(step(PAGE))),
            KeyCode::Home => self.relic_state.select(Some(0)),
            KeyCode::End => self.relic_state.select(Some(len - 1)),
            KeyCode::Char(' ') => {
                report.toggle(current);
                self.relic_state.select(Some(step(1)));
            }
            KeyCode::Char('a') => report.toggle_all(),
            KeyCode::Left | KeyCode::Char('h') => self.focus = Focus::Tree,
            KeyCode::Enter => {
                let path = report.relics[current].path.clone();
                self.go_to(path);
            }
            _ => {}
        }
    }

    // ---------------------------------------------------------------- scan

    fn start_scan(&mut self, then_clean: bool) {
        if self.clean.is_some() {
            return;
        }
        let Some(node) = self.tree.selected() else { return };
        let root = node.path.clone();
        if let Some(job) = self.scan.take() {
            job.cancel.store(true, Ordering::Relaxed);
        }
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        helpers::spawn_scan(root.clone(), self.tree.root_paths(), cancel.clone(), tx);
        self.scan = Some(ScanJob::new(rx, cancel, root.clone(), then_clean));
        self.report = Some(ScanReport::new(root));
        self.relic_state.select(None);
        self.focus = Focus::Tree;
    }

    fn pump_scan(&mut self) {
        let (Some(job), Some(report)) = (&mut self.scan, &mut self.report) else { return };
        let mut finished = None;
        loop {
            match job.rx.try_recv() {
                Ok(ScanEvent::Progress { dirs, current }) => {
                    report.dirs_scanned = dirs;
                    job.current = current;
                }
                Ok(ScanEvent::Project { path, languages }) => report.projects.push((path, languages)),
                Ok(ScanEvent::Relic(relic)) => report.insert_relic(relic),
                Ok(ScanEvent::Finished { dirs, cancelled }) => {
                    report.dirs_scanned = dirs;
                    finished = Some(cancelled);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    finished.get_or_insert(true);
                    break;
                }
            }
        }
        if self.relic_state.selected().is_none() && !report.relics.is_empty() {
            self.relic_state.select(Some(0));
        }
        let Some(cancelled) = finished else { return };

        report.finish(cancelled);
        let then_clean = job.then_clean && !cancelled;
        let (count, bytes, selected) = (report.relics.len(), report.bytes(None), report.selected_count());
        let summary = format!(
            "Scanned {} dirs in {} · {} relics · {}",
            helpers::human_count(report.dirs_scanned),
            helpers::human_duration(report.elapsed()),
            count,
            helpers::human_bytes(bytes)
        );
        self.scan = None;

        if cancelled {
            self.notify("Scan cancelled — showing partial results", Tone::Warn);
        } else if then_clean && selected > 0 {
            self.mode = Mode::Confirm;
        } else if then_clean {
            self.notify("✨ Spotless — nothing to clean here", Tone::Good);
        } else {
            self.notify(summary, Tone::Info);
        }
    }

    // ---------------------------------------------------------------- clean

    fn request_clean(&mut self) {
        let Some(node) = self.tree.selected() else { return };
        if let Some(job) = &mut self.scan {
            job.then_clean = true;
            self.notify("Clean dialog opens when the scan finishes", Tone::Info);
            return;
        }
        let fresh = self.report.as_ref().filter(|r| r.root == node.path && !r.running() && !r.cancelled);
        match fresh {
            Some(report) if report.selected_count() > 0 => self.mode = Mode::Confirm,
            Some(report) if report.relics.is_empty() => self.notify("✨ Spotless — nothing to clean here", Tone::Good),
            Some(_) => self.notify("Nothing marked — press Tab, then Space or a to mark relics", Tone::Warn),
            None => self.start_scan(true),
        }
    }

    fn start_clean(&mut self) {
        let Some(report) = &self.report else { return };
        let targets: Vec<(PathBuf, u64)> =
            report.relics.iter().filter(|r| r.selected).map(|r| (r.path.clone(), r.bytes)).collect();
        if targets.is_empty() {
            return;
        }
        let total_bytes = targets.iter().map(|(_, b)| b).sum();
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        self.clean = Some(CleanJob::new(rx, cancel.clone(), targets.len(), total_bytes));
        helpers::spawn_clean(targets, cancel, tx);
    }

    fn pump_clean(&mut self) {
        let Some(job) = &mut self.clean else { return };
        let mut finished = None;
        loop {
            match job.rx.try_recv() {
                Ok(CleanEvent::Working(path)) => job.current = path,
                Ok(CleanEvent::Removed { path, bytes }) => {
                    job.done += 1;
                    job.done_bytes += bytes;
                    job.freed += bytes;
                    if let Some(report) = &mut self.report {
                        report.remove_relic(&path);
                    }
                }
                Ok(CleanEvent::Failed { path, bytes, error }) => {
                    job.done += 1;
                    job.done_bytes += bytes;
                    job.failed += 1;
                    if let Some(report) = &mut self.report {
                        report.mark_failed(&path, error);
                    }
                }
                Ok(CleanEvent::Finished { cancelled }) => finished = Some(cancelled),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    finished.get_or_insert(true);
                    break;
                }
            }
        }
        let Some(cancelled) = finished else { return };

        let (removed, failed, freed, took) = (job.done - job.failed, job.failed, job.freed, job.started.elapsed());
        self.clean = None;
        self.tree.rebuild();
        self.disk.clear();
        self.refresh_disk();
        let remaining = self.report.as_ref().map_or(0, |r| r.relics.len());
        self.relic_state.select(remaining.checked_sub(1).map(|last| self.relic_state.selected().unwrap_or(0).min(last)));
        if remaining == 0 {
            self.focus = Focus::Tree;
        }

        let freed = helpers::human_bytes(freed);
        let took = helpers::human_duration(took);
        match (failed, cancelled) {
            (0, false) => self.notify(format!("✔ Freed {freed} · {removed} relics removed in {took}"), Tone::Good),
            (0, true) => self.notify(format!("Stopped · freed {freed} from {removed} relics"), Tone::Warn),
            _ => self.notify(
                format!("Freed {freed} · {removed} removed · {failed} failed (files in use?)"),
                Tone::Warn,
            ),
        }
    }
}
