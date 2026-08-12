use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::Layout;

/// A normalized action in the Reading state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Command {
    None,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    Quit,
}

impl Command {
    #[must_use]
    pub fn from_key(key: KeyEvent) -> Self {
        match (key.code, key.modifiers) {
            (KeyCode::Char('q') | KeyCode::Esc, _) => Self::Quit,
            (KeyCode::Up | KeyCode::Char('k'), _) => Self::Up,
            (KeyCode::Down | KeyCode::Char('j'), _) => Self::Down,
            (KeyCode::PageUp, _) | (KeyCode::Char('u'), KeyModifiers::CONTROL) => Self::PageUp,
            (KeyCode::PageDown | KeyCode::Char(' '), _)
            | (KeyCode::Char('d'), KeyModifiers::CONTROL) => Self::PageDown,
            (KeyCode::Home | KeyCode::Char('g'), _) => Self::Home,
            (KeyCode::End | KeyCode::Char('G'), _) => Self::End,
            _ => Self::None,
        }
    }
}

/// Reading-mode state. Source offsets, not display rows, are canonical.
#[derive(Clone, Debug, Default)]
pub struct App {
    top_source: usize,
    quit: bool,
}

impl App {
    #[must_use]
    pub fn top_source(&self) -> usize {
        self.top_source
    }

    #[must_use]
    pub fn should_quit(&self) -> bool {
        self.quit
    }

    pub fn apply(&mut self, command: Command, layout: &Layout, viewport_height: u16) {
        let rows = layout.rows();
        let current = layout.row_for_source(self.top_source);
        let page = usize::from(viewport_height.max(1));
        let last = rows.len().saturating_sub(1);
        let target = match command {
            Command::None => return,
            Command::Up => current.saturating_sub(1),
            Command::Down => current.saturating_add(1).min(last),
            Command::PageUp => current.saturating_sub(page),
            Command::PageDown => current.saturating_add(page).min(last),
            Command::Home => 0,
            Command::End => last,
            Command::Quit => {
                self.quit = true;
                return;
            }
        };
        if let Some(row) = rows.get(target) {
            self.top_source = row.source.start;
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::Document;

    use super::*;

    #[test]
    fn navigation_tracks_source_instead_of_row_number() {
        let document = Document::parse("one two three four five six seven eight".to_owned());
        let narrow = Layout::new(&document, 8);
        let mut app = App::default();
        app.apply(Command::Down, &narrow, 2);
        app.apply(Command::Down, &narrow, 2);
        let anchor = app.top_source();

        let wide = Layout::new(&document, 16);
        let restored = wide.row_for_source(anchor);

        assert!(anchor > 0);
        assert!(wide.rows()[restored].source.end >= anchor);
    }

    #[test]
    fn narrow_thematic_rule_does_not_trap_navigation() {
        let source = "---\n\nAfter the rule";
        let document = Document::parse(source.to_owned());
        let layout = Layout::new(&document, 1);
        let mut app = App::default();

        for _ in 0..layout.rows().len() {
            app.apply(Command::Down, &layout, 1);
        }

        let after = source
            .find("After")
            .expect("fixture contains trailing text");
        assert!(app.top_source() >= after);
    }

    #[test]
    fn narrow_synthetic_list_marker_does_not_trap_navigation() {
        let source = "- item";
        let document = Document::parse(source.to_owned());
        let layout = Layout::new(&document, 1);
        let mut app = App::default();

        for _ in 0..layout.rows().len() {
            app.apply(Command::Down, &layout, 1);
        }

        let item = source.find("item").expect("fixture contains list text");
        assert!(app.top_source() >= item);
    }

    #[test]
    fn narrow_task_marker_does_not_trap_navigation() {
        let source = "- [x] item";
        let document = Document::parse(source.to_owned());
        let layout = Layout::new(&document, 1);
        let mut app = App::default();

        for _ in 0..layout.rows().len() {
            app.apply(Command::Down, &layout, 1);
        }

        let item = source.find("item").expect("fixture contains task text");
        assert!(app.top_source() >= item);
    }
}
