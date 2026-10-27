use ratatui::style::Color;

/// The environment devclean runs in; drives which roots the explorer offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Platform {
    Windows,
    Wsl { distro: Option<String> },
    Linux,
    Other,
}

impl Platform {
    pub fn detect() -> Self {
        if cfg!(windows) {
            return Platform::Windows;
        }
        if cfg!(target_os = "linux") {
            let release = std::fs::read_to_string("/proc/sys/kernel/osrelease")
                .unwrap_or_default()
                .to_lowercase();
            let distro = std::env::var("WSL_DISTRO_NAME").ok();
            if release.contains("microsoft") || release.contains("wsl") || distro.is_some() {
                return Platform::Wsl { distro };
            }
            return Platform::Linux;
        }
        Platform::Other
    }

    pub fn label(&self) -> String {
        match self {
            Platform::Windows => "WINDOWS".into(),
            Platform::Wsl { distro } => format!("WSL · {}", distro.as_deref().unwrap_or("Linux")),
            Platform::Linux => "LINUX".into(),
            Platform::Other => "UNIX".into(),
        }
    }

    pub fn color(&self) -> Color {
        match self {
            Platform::Windows => Color::Rgb(0, 120, 212),
            Platform::Wsl { .. } => Color::Rgb(233, 84, 32),
            Platform::Linux => Color::Rgb(230, 190, 40),
            Platform::Other => Color::Gray,
        }
    }
}
