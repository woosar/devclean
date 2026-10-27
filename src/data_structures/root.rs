use std::path::PathBuf;

use ratatui::style::Color;

/// `Filesystem` and `Mount` only occur on Unix-like systems.
#[cfg_attr(windows, allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootKind {
    Home,
    Filesystem,
    Drive,
    Removable,
    Network,
    Mount,
    Pinned,
}

impl RootKind {
    pub fn name(self) -> &'static str {
        match self {
            RootKind::Home => "Home",
            RootKind::Filesystem => "Filesystem",
            RootKind::Drive => "Drive",
            RootKind::Removable => "Removable",
            RootKind::Network => "Network",
            RootKind::Mount => "Mount",
            RootKind::Pinned => "Pinned",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            RootKind::Home => "⌂",
            RootKind::Filesystem => "◆",
            RootKind::Drive => "▣",
            RootKind::Removable => "▢",
            RootKind::Network => "◎",
            RootKind::Mount => "◈",
            RootKind::Pinned => "★",
        }
    }

    pub fn color(self) -> Color {
        match self {
            RootKind::Home => Color::Rgb(110, 231, 183),
            RootKind::Filesystem => Color::Rgb(230, 190, 40),
            RootKind::Drive => Color::Rgb(96, 165, 250),
            RootKind::Removable => Color::Rgb(244, 114, 182),
            RootKind::Network => Color::Rgb(45, 212, 191),
            RootKind::Mount => Color::Rgb(233, 140, 80),
            RootKind::Pinned => Color::Rgb(250, 204, 21),
        }
    }
}

/// A top-level entry in the explorer: a drive, mount point, home folder, ...
#[derive(Debug, Clone)]
pub struct Root {
    pub path: PathBuf,
    pub label: String,
    pub kind: RootKind,
    pub fs_type: Option<String>,
    pub volume: Option<String>,
}
