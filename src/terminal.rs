use std::io::{self, IsTerminal, Stdout};
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use crossterm::event::{self, Event, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::{App, Command, Document, Layout, ui};

pub(crate) fn run(path: &Path, document: &Document) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(anyhow!(
            "an interactive terminal is required (stdin and stdout must both be terminals)"
        ));
    }

    let mut session = TerminalSession::enter()?;
    let run_result = run_loop(session.terminal_mut(), path, document);
    let restore_result = session.restore();
    combine_run_and_restore(run_result, restore_result)
}

fn combine_run_and_restore(run_result: Result<()>, restore_result: Result<()>) -> Result<()> {
    match (run_result, restore_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(run_error), Ok(())) => Err(run_error),
        (Ok(()), Err(restore_error)) => Err(restore_error),
        (Err(run_error), Err(restore_error)) => Err(anyhow!(
            "{run_error:#}; terminal cleanup also failed: {restore_error:#}"
        )),
    }
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    path: &Path,
    document: &Document,
) -> Result<()> {
    let mut app = App::default();
    while !app.should_quit() {
        let size = terminal.size().context("could not read terminal size")?;
        let body_height = size.height.saturating_sub(2).max(1);
        let layout = Layout::new(document, size.width);
        terminal
            .draw(|frame| ui::render(frame, path, &app, &layout))
            .context("could not draw the Markdown document")?;

        match event::read().context("could not read terminal input")? {
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                app.apply(Command::from_key(key), &layout, body_height);
            }
            Event::Resize(_, _) => {}
            _ => {}
        }
    }
    Ok(())
}

struct TerminalSession {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    cursor_needs_restoring: bool,
    alternate_screen_active: bool,
    raw_mode_active: bool,
}

impl TerminalSession {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("could not enable terminal raw mode")?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen) {
            return match cleanup_setup(&mut stdout) {
                Ok(()) => Err(error).context("could not enter alternate screen"),
                Err(cleanup_error) => Err(error).context(format!(
                    "could not enter alternate screen; cleanup also failed: {cleanup_error:#}"
                )),
            };
        }

        let backend = CrosstermBackend::new(stdout);
        let terminal = match Terminal::new(backend) {
            Ok(terminal) => terminal,
            Err(error) => {
                let mut stdout = io::stdout();
                return match cleanup_setup(&mut stdout) {
                    Ok(()) => Err(error).context("could not initialize terminal"),
                    Err(cleanup_error) => Err(error).context(format!(
                        "could not initialize terminal; cleanup also failed: {cleanup_error:#}"
                    )),
                };
            }
        };

        Ok(Self {
            terminal,
            cursor_needs_restoring: true,
            alternate_screen_active: true,
            raw_mode_active: true,
        })
    }

    fn terminal_mut(&mut self) -> &mut Terminal<CrosstermBackend<Stdout>> {
        &mut self.terminal
    }

    fn restore(&mut self) -> Result<()> {
        let mut failures = Vec::new();

        if self.cursor_needs_restoring {
            match self.terminal.show_cursor() {
                Ok(()) => self.cursor_needs_restoring = false,
                Err(error) => failures.push(format!("could not restore terminal cursor: {error}")),
            }
        }

        if self.alternate_screen_active {
            match execute!(self.terminal.backend_mut(), LeaveAlternateScreen) {
                Ok(()) => self.alternate_screen_active = false,
                Err(error) => failures.push(format!("could not leave alternate screen: {error}")),
            }
        }

        if self.raw_mode_active {
            match disable_raw_mode() {
                Ok(()) => self.raw_mode_active = false,
                Err(error) => {
                    failures.push(format!("could not disable terminal raw mode: {error}"))
                }
            }
        }

        if failures.is_empty() {
            Ok(())
        } else {
            Err(anyhow!(failures.join("; ")))
        }
    }
}

fn cleanup_setup(stdout: &mut Stdout) -> Result<()> {
    let mut failures = Vec::new();
    if let Err(error) = execute!(stdout, LeaveAlternateScreen) {
        failures.push(format!("could not leave alternate screen: {error}"));
    }
    if let Err(error) = disable_raw_mode() {
        failures.push(format!("could not disable terminal raw mode: {error}"));
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(anyhow!(failures.join("; ")))
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        if let Err(error) = self.restore() {
            eprintln!("raden: terminal cleanup failed: {error:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_both_run_and_cleanup_failures() {
        let error = combine_run_and_restore(
            Err(anyhow!("input failed")),
            Err(anyhow!("raw mode stayed active")),
        )
        .expect_err("both failures must be reported");
        let message = format!("{error:#}");

        assert!(message.contains("input failed"));
        assert!(message.contains("raw mode stayed active"));
    }
}
