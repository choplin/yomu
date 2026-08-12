use std::path::Path;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout as TuiLayout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;

use crate::{App, Layout, LayoutFragment, TextStyle};

pub(crate) fn render(frame: &mut Frame<'_>, path: &Path, app: &App, layout: &Layout) {
    let [header, body, footer] = TuiLayout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    let title = format!(" raden — {}", path.display());
    frame.render_widget(
        Paragraph::new(title).style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        header,
    );

    let first = layout.row_for_source(app.top_source());
    let visible = usize::from(body.height);
    let lines = layout
        .rows()
        .iter()
        .skip(first)
        .take(visible)
        .map(row_line)
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(Text::from(lines)), body);

    let total = layout.rows().len().max(1);
    let percent = first.saturating_mul(100) / total.saturating_sub(1).max(1);
    let status = format!(
        " j/k scroll  Ctrl-d/u page  g/G ends  q quit{:>8}% ",
        percent
    );
    frame.render_widget(
        Paragraph::new(status).style(Style::default().fg(Color::DarkGray)),
        footer,
    );
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
