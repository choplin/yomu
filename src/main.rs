use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

#[derive(Debug, Parser)]
#[command(version, about = "Read one Markdown file in the terminal")]
struct Cli {
    /// Local Markdown file to open.
    path: PathBuf,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match raden::run(&cli.path) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("raden: {error:#}");
            ExitCode::FAILURE
        }
    }
}
