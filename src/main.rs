mod data_structures;
mod helpers;

use std::path::PathBuf;

use data_structures::App;

fn main() -> std::io::Result<()> {
    let start = std::env::args_os().nth(1).map(PathBuf::from);
    ratatui::run(|terminal| App::new(start).run(terminal))
}
