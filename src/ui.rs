use std::path::Path;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout as TuiLayout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;

use crate::{App, Layout, LayoutFragment, TextStyle};

pub(crate) fn render(
    frame: &mut Frame<'_>,
    path: &Path,
    app: &App,
    layout: &Layout,
    graphics_status: &str,
    max_width: Option<u16>,
) {
    let [header, body, footer] = viewport_areas(frame.area(), max_width);

    let title = format!(" raden — {}", path.display());
    frame.render_widget(
        Paragraph::new(title).style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        header,
    );

    let (first, first_row_offset) = app.viewport_position(layout);
    let mut lines = Vec::new();
    for (visible_index, row) in layout.rows().iter().skip(first).enumerate() {
        if lines.len() >= usize::from(body.height) {
            break;
        }
        let row_offset = if visible_index == 0 {
            first_row_offset.min(row.height.saturating_sub(1))
        } else {
            0
        };
        lines.push(if row_offset == 0 {
            row_line(row)
        } else {
            Line::default()
        });
        for _ in row_offset.saturating_add(1)..row.height {
            if lines.len() >= usize::from(body.height) {
                break;
            }
            lines.push(Line::default());
        }
    }
    frame.render_widget(Paragraph::new(Text::from(lines)), body);

    let percent = app.viewport_percent(layout, body.height);
    let status = format!(
        " j/k scroll  Ctrl-d/u page  g/G ends  q quit  {graphics_status}{:>5}% ",
        percent,
    );
    frame.render_widget(
        Paragraph::new(status).style(Style::default().fg(Color::DarkGray)),
        footer,
    );
}

pub(crate) fn viewport_areas(area: Rect, max_width: Option<u16>) -> [Rect; 3] {
    let [header, mut body, footer] = TuiLayout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(area);
    if let Some(max_width) = max_width
        && body.width > max_width
    {
        body.x = body.x.saturating_add((body.width - max_width) / 2);
        body.width = max_width;
    }
    [header, body, footer]
}

fn row_line(row: &crate::LayoutRow) -> Line<'static> {
    Line::from(row.fragments.iter().map(fragment_span).collect::<Vec<_>>())
}

fn fragment_span(fragment: &LayoutFragment) -> Span<'static> {
    Span::styled(fragment.text.clone(), tui_style(fragment.style))
}

fn tui_style(source: TextStyle) -> Style {
    let mut style = Style::default();
    if let Some(level) = source.heading {
        style = style
            .fg(if level <= 2 { Color::Cyan } else { Color::Blue })
            .add_modifier(Modifier::BOLD);
    }
    if source.emphasis || source.quote {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if source.strong {
        style = style.add_modifier(Modifier::BOLD);
    }
    if source.strikethrough {
        style = style.add_modifier(Modifier::CROSSED_OUT);
    }
    if source.code {
        style = style.fg(Color::Yellow);
    }
    if source.link {
        style = style.fg(Color::Blue).add_modifier(Modifier::UNDERLINED);
    }
    if source.muted {
        style = style.fg(Color::DarkGray);
    }
    style
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_is_centered_at_the_configured_width() {
        let [header, body, footer] = viewport_areas(Rect::new(0, 0, 140, 24), Some(100));

        assert_eq!(body, Rect::new(20, 1, 100, 22));
        assert_eq!(header, Rect::new(0, 0, 140, 1));
        assert_eq!(footer, Rect::new(0, 23, 140, 1));
    }

    #[test]
    fn narrow_and_unbounded_bodies_use_the_available_width() {
        let [_, narrow, _] = viewport_areas(Rect::new(0, 0, 72, 12), Some(100));
        let [_, unbounded, _] = viewport_areas(Rect::new(0, 0, 140, 12), None);

        assert_eq!(narrow, Rect::new(0, 1, 72, 10));
        assert_eq!(unbounded, Rect::new(0, 1, 140, 10));
    }
}
