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
    top_row_offset: u16,
    quit: bool,
}

impl App {
    #[must_use]
    pub fn top_source(&self) -> usize {
        self.top_source
    }

    #[must_use]
    pub fn top_row_offset(&self) -> u16 {
        self.top_row_offset
    }

    #[must_use]
    pub fn viewport_position(&self, layout: &Layout) -> (usize, u16) {
        let row = layout.row_for_source(self.top_source);
        let offset = self
            .top_row_offset
            .min(layout.rows()[row].height.saturating_sub(1));
        (row, offset)
    }

    #[must_use]
    pub fn should_quit(&self) -> bool {
        self.quit
    }

    pub fn apply(&mut self, command: Command, layout: &Layout, viewport_height: u16) {
        let rows = layout.rows();
        let (current, offset) = self.viewport_position(layout);
        let page = usize::from(viewport_height.max(1));
        let last = rows.len().saturating_sub(1);
        let (target, target_offset) = match command {
            Command::None => return,
            Command::Up if offset > 0 => (current, offset - 1),
            Command::Up if current > 0 => {
                let previous = current - 1;
                (previous, rows[previous].height.saturating_sub(1))
            }
            Command::Up => (0, 0),
            Command::Down if offset + 1 < rows[current].height => (current, offset + 1),
            Command::Down => (current.saturating_add(1).min(last), 0),
            Command::PageUp => move_up_cells(rows, current, offset, page),
            Command::PageDown => move_down_cells(rows, current, offset, page),
            Command::Home => (0, 0),
            Command::End => (last, 0),
            Command::Quit => {
                self.quit = true;
                return;
            }
        };
        if let Some(row) = rows.get(target) {
            self.top_source = row.source.start;
            self.top_row_offset = target_offset;
        }
    }
}

fn move_down_cells(
    rows: &[crate::LayoutRow],
    mut row: usize,
    mut offset: u16,
    mut cells: usize,
) -> (usize, u16) {
    while cells > 0 {
        if offset + 1 < rows[row].height {
            offset += 1;
        } else if row + 1 < rows.len() {
            row += 1;
            offset = 0;
        } else {
            break;
        }
        cells -= 1;
    }
    (row, offset)
}

fn move_up_cells(
    rows: &[crate::LayoutRow],
    mut row: usize,
    mut offset: u16,
    mut cells: usize,
) -> (usize, u16) {
    while cells > 0 {
        if offset > 0 {
            offset -= 1;
        } else if row > 0 {
            row -= 1;
            offset = rows[row].height.saturating_sub(1);
        } else {
            break;
        }
        cells -= 1;
    }
    (row, offset)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use image::{ImageBuffer, Rgba};

    use crate::image::{ImageCatalog, RasterCache};
    use crate::{Document, PixelSize};

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
    fn image_rows_scroll_one_cell_in_both_directions() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("raden-app-image-{suffix}"));
        fs::create_dir(&root).expect("fixture directory should be created");
        let image_path = root.join("image.png");
        ImageBuffer::<Rgba<u8>, _>::from_pixel(4, 4, Rgba([1, 2, 3, 255]))
            .save(&image_path)
            .expect("fixture should be saved");
        let document = Document::parse("before\n\n![image](image.png)\n\nafter".to_owned());
        let catalog = ImageCatalog::load(&document, &root.join("doc.md"));
        let mut cache = RasterCache::new(1_024);
        let layout = Layout::with_graphics(
            &document,
            10,
            10,
            &catalog,
            &mut cache,
            PixelSize {
                width: 1,
                height: 1,
            },
            0,
        );
        let graphic_row = layout
            .rows()
            .iter()
            .position(|row| row.height == 4)
            .expect("fixture should contain a four-cell image row");
        let mut app = App::default();
        while app.viewport_position(&layout).0 < graphic_row {
            app.apply(Command::Down, &layout, 10);
        }

        assert_eq!(app.viewport_position(&layout), (graphic_row, 0));
        app.apply(Command::Down, &layout, 10);
        assert_eq!(app.viewport_position(&layout), (graphic_row, 1));
        app.apply(Command::Down, &layout, 10);
        assert_eq!(app.viewport_position(&layout), (graphic_row, 2));
        app.apply(Command::Up, &layout, 10);
        assert_eq!(app.viewport_position(&layout), (graphic_row, 1));

        fs::remove_file(image_path).expect("fixture should be removable");
        fs::remove_dir(root).expect("fixture directory should be removable");
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
