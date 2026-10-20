use urushi_tui_app::{KeyCode, KeyEvent};

use crate::{Layout, SourceSpan};

/// How a search query compares letter case.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum SearchCase {
    #[default]
    Smart,
    Sensitive,
    Insensitive,
}

impl SearchCase {
    fn is_sensitive(self, query: &str) -> bool {
        match self {
            Self::Smart => query.chars().any(char::is_uppercase),
            Self::Sensitive => true,
            Self::Insensitive => false,
        }
    }
}

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
}

impl Command {
    #[must_use]
    pub fn from_key(key: KeyEvent) -> Self {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => Self::Up,
            KeyCode::Down | KeyCode::Char('j') => Self::Down,
            KeyCode::PageUp | KeyCode::Char('u') => Self::PageUp,
            KeyCode::PageDown | KeyCode::Char(' ' | 'd') => Self::PageDown,
            KeyCode::Home | KeyCode::Char('g') => Self::Home,
            KeyCode::End | KeyCode::Char('G') => Self::End,
            _ => Self::None,
        }
    }
}

/// Reading-mode state. Source offsets, not display rows, are canonical.
#[derive(Clone, Debug, Default)]
pub struct App {
    top_source: usize,
    top_source_end: usize,
    top_row_offset: u16,
    search: SearchState,
}

#[derive(Clone, Debug, Default)]
struct SearchState {
    query: String,
    matches: Vec<SourceSpan>,
    current: Option<usize>,
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
    pub(crate) fn search_status(&self) -> Option<(&str, Option<(usize, usize)>)> {
        (!self.search.query.is_empty()).then(|| {
            (
                self.search.query.as_str(),
                self.search
                    .current
                    .map(|current| (current + 1, self.search.matches.len())),
            )
        })
    }

    #[must_use]
    pub(crate) fn current_match(&self) -> Option<&SourceSpan> {
        self.search
            .current
            .and_then(|current| self.search.matches.get(current))
    }

    pub(crate) fn search(&mut self, source: &str, query: String, case: SearchCase) {
        let matches = search_matches(source, &query, case);
        let current = matches
            .iter()
            .position(|matched| matched.start >= self.top_source)
            .or_else(|| (!matches.is_empty()).then_some(0));
        self.search = SearchState {
            query,
            matches,
            current,
        };
        self.jump_to_current_match();
    }

    pub(crate) fn next_match(&mut self) {
        if let Some(current) = self.search.current {
            self.search.current = Some((current + 1) % self.search.matches.len());
            self.jump_to_current_match();
        }
    }

    pub(crate) fn previous_match(&mut self) {
        if let Some(current) = self.search.current {
            self.search.current = Some(if current == 0 {
                self.search.matches.len() - 1
            } else {
                current - 1
            });
            self.jump_to_current_match();
        }
    }

    #[must_use]
    pub fn viewport_position(&self, layout: &Layout) -> (usize, u16) {
        let row = layout.row_for_anchor(&(self.top_source..self.top_source_end));
        let offset = self
            .top_row_offset
            .min(layout.rows()[row].height.saturating_sub(1));
        (row, offset)
    }

    #[must_use]
    pub(crate) fn viewport_percent(&self, layout: &Layout, viewport_height: u16) -> usize {
        let rows = layout.rows();
        let (row, offset) = self.viewport_position(layout);
        let maximum = maximum_scroll_cell(rows, viewport_height);
        cell_position(rows, row, offset)
            .min(maximum)
            .saturating_mul(100)
            .checked_div(maximum)
            .unwrap_or(100)
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
            Command::End => (last, rows[last].height.saturating_sub(1)),
        };
        self.set_position(
            rows,
            clamp_position(rows, target, target_offset, viewport_height),
        );
    }

    pub(crate) fn clamp_viewport(&mut self, layout: &Layout, viewport_height: u16) {
        let rows = layout.rows();
        let position = self.viewport_position(layout);
        let clamped = clamp_position(rows, position.0, position.1, viewport_height);
        if clamped == position {
            self.top_row_offset = position.1;
        } else {
            self.set_position(rows, clamped);
        }
    }

    fn set_position(&mut self, rows: &[crate::LayoutRow], (row, offset): (usize, u16)) {
        if let Some(row) = rows.get(row) {
            self.top_source = row.source.start;
            self.top_source_end = row.source.end;
            self.top_row_offset = offset;
        }
    }

    fn jump_to_current_match(&mut self) {
        if let Some(matched) = self.current_match().cloned() {
            self.top_source = matched.start;
            self.top_source_end = matched.end;
            self.top_row_offset = 0;
        }
    }
}

fn search_matches(source: &str, query: &str, case: SearchCase) -> Vec<SourceSpan> {
    if query.is_empty() {
        return Vec::new();
    }
    if case.is_sensitive(query) {
        return source
            .match_indices(query)
            .map(|(start, matched)| start..start + matched.len())
            .collect();
    }

    let source_chars = source.char_indices().collect::<Vec<_>>();
    let query_chars = query.chars().collect::<Vec<_>>();
    source_chars
        .windows(query_chars.len())
        .filter(|candidate| {
            candidate
                .iter()
                .map(|(_, character)| character)
                .zip(&query_chars)
                .all(|(source, query)| source.to_lowercase().eq(query.to_lowercase()))
        })
        .map(|candidate| {
            let start = candidate[0].0;
            let (last_start, last_character) = candidate[candidate.len() - 1];
            start..last_start + last_character.len_utf8()
        })
        .collect()
}

fn clamp_position(
    rows: &[crate::LayoutRow],
    row: usize,
    offset: u16,
    viewport_height: u16,
) -> (usize, u16) {
    let requested = cell_position(rows, row, offset);
    position_for_cell(
        rows,
        requested.min(maximum_scroll_cell(rows, viewport_height)),
    )
}

fn maximum_scroll_cell(rows: &[crate::LayoutRow], viewport_height: u16) -> usize {
    rows.iter()
        .fold(0usize, |height, row| {
            height.saturating_add(usize::from(row.height))
        })
        .saturating_sub(usize::from(viewport_height.max(1)))
}

fn cell_position(rows: &[crate::LayoutRow], row: usize, offset: u16) -> usize {
    rows.iter()
        .take(row)
        .fold(0usize, |position, row| {
            position.saturating_add(usize::from(row.height))
        })
        .saturating_add(usize::from(offset))
}

fn position_for_cell(rows: &[crate::LayoutRow], mut cell: usize) -> (usize, u16) {
    for (row_index, row) in rows.iter().enumerate() {
        let height = usize::from(row.height);
        if cell < height {
            return (row_index, cell as u16);
        }
        cell = cell.saturating_sub(height);
    }
    let last = rows.len().saturating_sub(1);
    (last, rows[last].height.saturating_sub(1))
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

    fn remaining_cells(layout: &Layout, (row, offset): (usize, u16)) -> usize {
        usize::from(layout.rows()[row].height.saturating_sub(offset))
            + layout
                .rows()
                .iter()
                .skip(row + 1)
                .map(|row| usize::from(row.height))
                .sum::<usize>()
    }

    fn viewport_contains_source(
        layout: &Layout,
        position: (usize, u16),
        viewport_height: u16,
        source: usize,
    ) -> bool {
        let (first, offset) = position;
        let target = layout.row_for_source(source);
        if target < first {
            return false;
        }
        let cells_before_target = usize::from(layout.rows()[first].height.saturating_sub(offset))
            + layout
                .rows()
                .iter()
                .skip(first + 1)
                .take(target.saturating_sub(first + 1))
                .map(|row| usize::from(row.height))
                .sum::<usize>();
        target == first || cells_before_target < usize::from(viewport_height)
    }

    #[test]
    fn page_scrolling_binds_to_unmodified_d_and_u() {
        let key = |code| Command::from_key(KeyEvent::new(code));

        assert_eq!(key(KeyCode::Char('u')), Command::PageUp);
        assert_eq!(key(KeyCode::Char('d')), Command::PageDown);
        assert_eq!(key(KeyCode::PageUp), Command::PageUp);
        assert_eq!(key(KeyCode::PageDown), Command::PageDown);
        assert_eq!(key(KeyCode::Char(' ')), Command::PageDown);
    }

    #[test]
    fn search_starts_at_the_next_match_and_wraps_in_both_directions() {
        let source = "before needle middle needle after";
        let mut app = App {
            top_source: 14,
            top_source_end: 14,
            ..App::default()
        };

        app.search(source, "needle".to_owned(), SearchCase::Sensitive);
        assert_eq!(app.current_match(), Some(&(21..27)));
        assert_eq!(app.search_status(), Some(("needle", Some((2, 2)))));

        app.next_match();
        assert_eq!(app.current_match(), Some(&(7..13)));
        app.previous_match();
        assert_eq!(app.current_match(), Some(&(21..27)));
    }

    #[test]
    fn missing_search_keeps_the_viewport_and_reports_no_match() {
        let source = "one two three four five six";
        let document = Document::parse(source.to_owned());
        let layout = Layout::new(&document, 5);
        let mut app = App::default();
        app.apply(Command::Down, &layout, 2);
        let position = app.viewport_position(&layout);

        app.search(source, "absent".to_owned(), SearchCase::Smart);

        assert_eq!(app.viewport_position(&layout), position);
        assert_eq!(app.search_status(), Some(("absent", None)));
        assert_eq!(app.current_match(), None);
    }

    #[test]
    fn smart_case_folds_lowercase_queries_and_respects_uppercase_queries() {
        let source = "Alpha ALPHA alpha Äpfel äPFEL";
        assert_eq!(
            search_matches(source, "alpha", SearchCase::Smart),
            vec![0..5, 6..11, 12..17]
        );
        assert_eq!(
            search_matches(source, "Alpha", SearchCase::Smart),
            vec![0..5]
        );
        assert_eq!(
            search_matches(source, "äpfel", SearchCase::Smart),
            vec![18..24, 25..31]
        );
    }

    #[test]
    fn explicit_case_modes_override_smart_case() {
        let source = "Needle needle";

        assert_eq!(
            search_matches(source, "Needle", SearchCase::Insensitive),
            vec![0..6, 7..13]
        );
        assert_eq!(
            search_matches(source, "needle", SearchCase::Sensitive),
            vec![7..13]
        );
    }

    #[test]
    fn short_document_never_scrolls_beyond_its_initial_viewport() {
        let document = Document::parse("short document".to_owned());
        let layout = Layout::new(&document, 80);
        let mut app = App::default();

        for command in [Command::Down, Command::PageDown, Command::End] {
            app.apply(command, &layout, 10);
        }

        assert_eq!(app.viewport_position(&layout), (0, 0));
        assert_eq!(app.viewport_percent(&layout, 10), 100);
    }

    #[test]
    fn down_and_page_down_stop_at_the_same_last_useful_viewport() {
        let source = (0..12)
            .map(|index| format!("paragraph {index}\n\n"))
            .collect::<String>();
        let document = Document::parse(source);
        let layout = Layout::new(&document, 80);
        let mut row_scrolling = App::default();
        let mut page_scrolling = App::default();

        for _ in 0..100 {
            row_scrolling.apply(Command::Down, &layout, 4);
            page_scrolling.apply(Command::PageDown, &layout, 4);
        }

        assert_eq!(
            row_scrolling.viewport_position(&layout),
            page_scrolling.viewport_position(&layout)
        );
        assert_eq!(
            remaining_cells(&layout, row_scrolling.viewport_position(&layout)),
            4
        );
        assert_eq!(row_scrolling.viewport_percent(&layout, 4), 100);
    }

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
        let root = std::env::temp_dir().join(format!("yomu-app-image-{suffix}"));
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
            app.apply(Command::Down, &layout, 2);
        }

        assert_eq!(app.viewport_position(&layout), (graphic_row, 0));
        app.apply(Command::Down, &layout, 2);
        assert_eq!(app.viewport_position(&layout), (graphic_row, 1));
        app.apply(Command::Down, &layout, 2);
        assert_eq!(app.viewport_position(&layout), (graphic_row, 2));
        app.apply(Command::Up, &layout, 2);
        assert_eq!(app.viewport_position(&layout), (graphic_row, 1));

        let mut page_app = App::default();
        for _ in 0..32 {
            app.apply(Command::Down, &layout, 2);
            page_app.apply(Command::PageDown, &layout, 2);
        }
        assert_eq!(
            app.viewport_position(&layout),
            page_app.viewport_position(&layout)
        );
        assert_eq!(remaining_cells(&layout, app.viewport_position(&layout)), 2);

        fs::remove_file(image_path).expect("fixture should be removable");
        fs::remove_dir(root).expect("fixture directory should be removable");
    }

    #[test]
    fn navigation_leaves_a_graphic_followed_by_a_blank_line() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("yomu-app-image-exit-{suffix}"));
        fs::create_dir(&root).expect("fixture directory should be created");
        let image_path = root.join("image.png");
        ImageBuffer::<Rgba<u8>, _>::from_pixel(4, 4, Rgba([1, 2, 3, 255]))
            .save(&image_path)
            .expect("fixture should be saved");
        let source = "before\n\n![image](image.png)\n\nafter the image";
        let document = Document::parse(source.to_owned());
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
        let after = source
            .find("after")
            .expect("fixture contains trailing text");
        let mut app = App::default();

        for _ in 0..32 {
            app.apply(Command::Down, &layout, 2);
        }

        fs::remove_file(image_path).expect("fixture should be removable");
        fs::remove_dir(root).expect("fixture directory should be removable");
        assert!(
            app.top_source() >= after,
            "navigation stopped at source {} before trailing source {after}; rows: {:?}",
            app.top_source(),
            layout
                .rows()
                .iter()
                .map(|row| (row.source.clone(), row.height))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn navigation_progresses_when_graphics_layout_is_rebuilt_around_the_anchor() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/showcase.md");
        let source = fs::read_to_string(&path).expect("showcase fixture should be readable");
        let document = Document::parse(source.clone());
        let catalog = ImageCatalog::load(&document, &path);
        let cell = PixelSize {
            width: 8,
            height: 16,
        };
        let mut cache = RasterCache::default();
        let mut layout = Layout::with_graphics(&document, 80, 22, &catalog, &mut cache, cell, 0);
        let target = source
            .find("## Navigation checkpoint")
            .expect("showcase contains the navigation checkpoint");
        let mut app = App::default();

        for _ in 0..1_000 {
            app.apply(Command::Down, &layout, 22);
            layout = Layout::with_graphics(
                &document,
                80,
                22,
                &catalog,
                &mut cache,
                cell,
                app.top_source(),
            );
            if viewport_contains_source(&layout, app.viewport_position(&layout), 22, target) {
                break;
            }
        }

        assert!(
            viewport_contains_source(&layout, app.viewport_position(&layout), 22, target),
            "navigation stopped at source {} before checkpoint {target} became visible; nearby rows: {:?}",
            app.top_source(),
            layout
                .rows()
                .iter()
                .filter(|row| row.source.end >= 1_450 && row.source.start <= 1_600)
                .map(|row| (row.source.clone(), row.height))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn upward_navigation_progresses_when_graphics_layout_is_rebuilt_around_the_anchor() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/showcase.md");
        let source = fs::read_to_string(&path).expect("showcase fixture should be readable");
        let document = Document::parse(source);
        let catalog = ImageCatalog::load(&document, &path);
        let cell = PixelSize {
            width: 8,
            height: 16,
        };
        let mut cache = RasterCache::default();
        let mut layout = Layout::with_graphics(&document, 80, 22, &catalog, &mut cache, cell, 0);
        let mut app = App::default();
        app.apply(Command::End, &layout, 22);

        for _ in 0..1_000 {
            app.apply(Command::Up, &layout, 22);
            layout = Layout::with_graphics(
                &document,
                80,
                22,
                &catalog,
                &mut cache,
                cell,
                app.top_source(),
            );
            if app.top_source() == 0 {
                break;
            }
        }

        assert_eq!(
            app.top_source(),
            0,
            "upward navigation stopped before the start of the showcase"
        );
    }

    #[test]
    fn resizing_near_the_end_reclamps_to_a_valid_source_anchored_position() {
        let source = (0..20)
            .map(|index| format!("wrapped paragraph number {index}\n\n"))
            .collect::<String>();
        let document = Document::parse(source);
        let narrow = Layout::new(&document, 12);
        let wide = Layout::new(&document, 40);
        let mut app = App::default();
        app.apply(Command::End, &narrow, 4);

        app.clamp_viewport(&wide, 8);

        let (row, offset) = app.viewport_position(&wide);
        assert_eq!(app.top_source, wide.rows()[row].source.start);
        assert_eq!(app.top_source_end, wide.rows()[row].source.end);
        assert!(offset < wide.rows()[row].height);
        assert_eq!(remaining_cells(&wide, (row, offset)), 8);
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

    #[test]
    fn navigation_crosses_a_full_width_rule_in_both_directions() {
        let source = "before\n\n---\n\nafter the rule";
        let document = Document::parse(source.to_owned());
        let layout = Layout::new(&document, 40);
        let after = source
            .find("after")
            .expect("fixture contains trailing text");
        let mut app = App::default();

        for _ in 0..layout.rows().len() {
            app.apply(Command::Down, &layout, 2);
        }
        assert!(
            app.top_source() >= after,
            "downward motion must clear the rule"
        );

        for _ in 0..layout.rows().len() {
            app.apply(Command::Up, &layout, 2);
        }
        assert_eq!(app.top_source(), 0, "upward motion must clear the rule");
    }
}
