use std::path::Path;

use urushi::{
    Align, BlockStyle, Color, Length, Overflow, StyledText, TextSpan, TextStyle as UrushiTextStyle,
    View,
};
use urushi_tui_app::SurfaceSize;

use crate::{App, CellRect, Layout, LayoutFragment, TextStyle};

pub(crate) fn view(
    path: &Path,
    app: &App,
    layout: &Layout,
    graphics_status: &str,
    surface: SurfaceSize,
    max_width: Option<u16>,
) -> View {
    let [header, body, footer] = viewport_areas(surface, max_width);
    let title = format!(" raden — {}", path.display());
    let percent = app.viewport_percent(layout, body.height);
    let status =
        format!(" j/k scroll  d/u page  g/G ends  q quit  {graphics_status}{percent:>5}% ",);

    View::column(
        Align::Center,
        [
            fixed_block(
                header.width,
                header.height,
                View::text(title, UrushiTextStyle::new().foreground(Color::CYAN).bold()),
            ),
            fixed_block(body.width, body.height, body_view(app, layout, body.height)),
            fixed_block(
                footer.width,
                footer.height,
                View::text(
                    status,
                    UrushiTextStyle::new().foreground(Color::BRIGHT_BLACK),
                ),
            ),
        ],
    )
}

pub(crate) fn viewport_areas(surface: SurfaceSize, max_width: Option<u16>) -> [CellRect; 3] {
    let width = u16::try_from(surface.columns()).unwrap_or(u16::MAX);
    let height = u16::try_from(surface.rows()).unwrap_or(u16::MAX);
    let body_height = height.saturating_sub(2);
    let body_width = max_width.map_or(width, |maximum| width.min(maximum));
    let body_x = width.saturating_sub(body_width) / 2;
    let footer_y = height.saturating_sub(1);
    [
        CellRect {
            x: 0,
            y: 0,
            width,
            height: height.min(1),
        },
        CellRect {
            x: body_x,
            y: height.min(1),
            width: body_width,
            height: body_height,
        },
        CellRect {
            x: 0,
            y: footer_y,
            width,
            height: u16::from(height >= 2),
        },
    ]
}

fn fixed_block(width: u16, height: u16, child: View) -> View {
    View::block(
        BlockStyle::new()
            .width(Length::Cells(width))
            .height(Length::Cells(height))
            .overflow(Overflow::clip()),
        child,
    )
}

fn body_view(app: &App, layout: &Layout, height: u16) -> View {
    let (first, first_row_offset) = app.viewport_position(layout);
    let mut spans = Vec::new();
    let mut lines = 0usize;
    for (visible_index, row) in layout.rows().iter().skip(first).enumerate() {
        if lines >= usize::from(height) {
            break;
        }
        let row_offset = if visible_index == 0 {
            first_row_offset.min(row.height.saturating_sub(1))
        } else {
            0
        };
        if row_offset == 0 {
            spans.extend(row.fragments.iter().map(fragment_span));
        }
        spans.push(TextSpan::from("\n"));
        lines += 1;
        for _ in row_offset.saturating_add(1)..row.height {
            if lines >= usize::from(height) {
                break;
            }
            spans.push(TextSpan::from("\n"));
            lines += 1;
        }
    }
    let text = StyledText::try_from_spans(spans)
        .expect("layout fragment boundaries must preserve grapheme clusters");
    View::styled_text(text)
}

fn fragment_span(fragment: &LayoutFragment) -> TextSpan {
    TextSpan::new(fragment.text.clone(), urushi_style(fragment.style))
}

fn urushi_style(source: TextStyle) -> UrushiTextStyle {
    let mut style = UrushiTextStyle::new();
    if let Some(level) = source.heading {
        style = style
            .foreground(if level <= 2 { Color::CYAN } else { Color::BLUE })
            .bold();
    }
    if source.emphasis || source.quote {
        style = style.italic();
    }
    if source.strong {
        style = style.bold();
    }
    if source.strikethrough {
        style = style.strikethrough();
    }
    if source.code {
        style = style.foreground(Color::YELLOW);
    }
    if source.link {
        style = style.foreground(Color::BLUE).underlined();
    }
    if source.muted {
        style = style.foreground(Color::BRIGHT_BLACK);
    }
    if let Some(color) = source.foreground {
        style = style.foreground(Color::Rgb(color.red, color.green, color.blue));
    }
    style
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_is_centered_at_the_configured_width() {
        let [header, body, footer] = viewport_areas(SurfaceSize::new(140, 24), Some(100));

        assert_eq!(
            body,
            CellRect {
                x: 20,
                y: 1,
                width: 100,
                height: 22
            }
        );
        assert_eq!(
            header,
            CellRect {
                x: 0,
                y: 0,
                width: 140,
                height: 1
            }
        );
        assert_eq!(
            footer,
            CellRect {
                x: 0,
                y: 23,
                width: 140,
                height: 1
            }
        );
    }

    #[test]
    fn narrow_and_unbounded_bodies_use_the_available_width() {
        let [_, narrow, _] = viewport_areas(SurfaceSize::new(72, 12), Some(100));
        let [_, unbounded, _] = viewport_areas(SurfaceSize::new(140, 12), None);

        assert_eq!(
            narrow,
            CellRect {
                x: 0,
                y: 1,
                width: 72,
                height: 10
            }
        );
        assert_eq!(
            unbounded,
            CellRect {
                x: 0,
                y: 1,
                width: 140,
                height: 10
            }
        );
    }
}
