use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use raden::GraphicsPreference;

#[derive(Debug, Parser)]
#[command(version, about = "Read one Markdown file in the terminal")]
struct Cli {
    /// Local Markdown file to open.
    path: PathBuf,

    /// Graphics backend selection. Auto prefers Kitty, then sixel, then text.
    #[arg(long, value_enum)]
    graphics: Option<GraphicsPreference>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match raden::run_with_graphics(&cli.path, cli.graphics) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("raden: {error:#}");
            ExitCode::FAILURE
        }
    }
}
