use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, anyhow};
use clap::Parser;
use raden::GraphicsPreference;

#[derive(Debug, Parser)]
#[command(version, about = "Read Markdown in the terminal")]
struct Cli {
    /// Local Markdown file to open, or - for standard input.
    ///
    /// When PATH is omitted, raden reads standard input if it is redirected.
    #[arg(value_name = "PATH")]
    path: Option<PathBuf>,

    /// Graphics backend selection. Auto prefers Kitty, then sixel, then text.
    #[arg(long, value_enum)]
    graphics: Option<GraphicsPreference>,

    /// Theme name, such as builtin:dark, builtin:light, builtin:plain, or a user Theme.
    #[arg(long)]
    theme: Option<String>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("raden: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.path {
        Some(path) if path.as_os_str() != "-" => {
            raden::run_with_options(&path, cli.graphics, cli.theme.as_deref())
        }
        Some(_) => run_stdin(cli.graphics, cli.theme.as_deref()),
        None if !io::stdin().is_terminal() => run_stdin(cli.graphics, cli.theme.as_deref()),
        None => Err(anyhow!(
            "no Markdown input; pass a file path or pipe Markdown to standard input"
        )),
    }
}

fn run_stdin(graphics: Option<GraphicsPreference>, theme: Option<&str>) -> Result<()> {
    let mut source = String::new();
    io::stdin()
        .lock()
        .read_to_string(&mut source)
        .context("could not read Markdown from standard input")?;
    raden::run_markdown_with_options(&source, graphics, theme)
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn path_is_optional_for_redirected_standard_input() {
        let cli = Cli::try_parse_from(["raden"]).expect("omitted path should parse");

        assert_eq!(cli.path, None);
    }

    #[test]
    fn dash_selects_standard_input() {
        let cli = Cli::try_parse_from(["raden", "-"]).expect("dash should parse as a path");

        assert_eq!(cli.path, Some(PathBuf::from("-")));
    }

    #[test]
    fn theme_override_is_parsed() {
        let cli = Cli::try_parse_from(["raden", "--theme", "builtin:plain", "doc.md"])
            .expect("Theme override should parse");

        assert_eq!(cli.theme.as_deref(), Some("builtin:plain"));
    }
}
