use ratatui::style::Color;

/// Ecosystems whose build output devclean knows how to recognise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Language {
    Rust,
    DotNet,
    Python,
    Node,
}

impl Language {
    pub const ALL: [Language; 4] = [Language::Rust, Language::DotNet, Language::Python, Language::Node];

    pub fn name(self) -> &'static str {
        match self {
            Language::Rust => "Rust",
            Language::DotNet => "C# / .NET",
            Language::Python => "Python",
            Language::Node => "JS / TS",
        }
    }

    pub fn tag(self) -> &'static str {
        match self {
            Language::Rust => "rs",
            Language::DotNet => "cs",
            Language::Python => "py",
            Language::Node => "js",
        }
    }

    pub fn color(self) -> Color {
        match self {
            Language::Rust => Color::Rgb(240, 120, 60),
            Language::DotNet => Color::Rgb(170, 125, 255),
            Language::Python => Color::Rgb(90, 175, 235),
            Language::Node => Color::Rgb(240, 210, 70),
        }
    }
}
