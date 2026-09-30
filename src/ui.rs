use std::path::Path;

use unicode_width::UnicodeWidthChar;
use urushi::{
    Align, BlockStyle, Color, Key, Length, Overflow, Projection, ProjectionBoundary, StyledText,
    TextSpan, TextStyle as UrushiTextStyle, VerticalAlign, View, Viewport,
};
use urushi_graphics::{CellSize, Image, ImagePresentation, PixelSize as GraphicsPixelSize};
use urushi_tui_app::SurfaceSize;

use crate::{App, CellRect, Layout, LayoutRow, TextStyle};

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

fn body_view(app: &App, layout: &Layout, _height: u16) -> View {
    let (first, offset) = app.viewport_position(layout);
    let origin = layout
        .rows()
        .iter()
        .take(first)
        .fold(usize::from(offset), |origin, row| {
            origin.saturating_add(usize::from(row.height))
        });
    let document = View::column(
        Align::Left,
        layout.rows().iter().map(|row| layout_row_view(layout, row)),
    );
    View::viewport(
        Viewport::vertical(Projection::new(
            i64::try_from(origin).unwrap_or(i64::MAX),
            ProjectionBoundary::Preserve,
        )),
        document,
    )
}

fn layout_row_view(layout: &Layout, row: &LayoutRow) -> View {
    let mut graphics = layout.graphics_for_row(row).collect::<Vec<_>>();
    graphics.sort_by_key(|graphic| graphic.column);
    if graphics.is_empty() {
        return fixed_block(
            u16::try_from(layout.width()).unwrap_or(u16::MAX),
            row.height,
            row_text(row, 0, layout.width()),
        );
    }

    let mut children = Vec::new();
    let mut column = 0usize;
    for graphic in graphics {
        let graphic_column = usize::from(graphic.column);
        if graphic_column > column {
            children.push(fixed_block(
                u16::try_from(graphic_column - column).unwrap_or(u16::MAX),
                row.height,
                row_text(row, column, graphic_column),
            ));
        }
        let image = Image::rgba(
            Key::of(&graphic.element_key),
            Key::of(&graphic.asset.key),
            GraphicsPixelSize::new(
                graphic.asset.key.pixels.width,
                graphic.asset.key.pixels.height,
            ),
            graphic.asset.rgba.clone(),
        )
        .expect("raden raster assets contain checked RGBA pixels")
        .fallback(graphic.fallback.clone());
        children.push(
            ImagePresentation::new()
                .fallback_style(UrushiTextStyle::new().foreground(Color::BRIGHT_BLACK))
                .compose(&image, CellSize::new(graphic.columns, graphic.rows)),
        );
        column = graphic_column.saturating_add(usize::from(graphic.columns));
    }
    if column < layout.width() {
        children.push(fixed_block(
            u16::try_from(layout.width() - column).unwrap_or(u16::MAX),
            row.height,
            row_text(row, column, layout.width()),
        ));
    }

    fixed_block(
        u16::try_from(layout.width()).unwrap_or(u16::MAX),
        row.height,
        View::row(VerticalAlign::Top, children),
    )
}

fn row_text(row: &LayoutRow, start: usize, end: usize) -> View {
    let mut spans = Vec::new();
    let mut column = 0usize;
    for fragment in &row.fragments {
        let style = urushi_style(fragment.style);
        let mut selected = String::new();
        for character in fragment.text.chars() {
            let width = character.width().unwrap_or(0);
            let next = column.saturating_add(width);
            if column >= start && next <= end {
                selected.push(character);
            }
            column = next;
        }
        if !selected.is_empty() {
            spans.push(TextSpan::new(selected, style));
        }
    }
    if spans.is_empty() {
        View::empty()
    } else {
        View::styled_text(
            StyledText::try_from_spans(spans)
                .expect("layout fragment boundaries preserve grapheme clusters"),
        )
    }
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
    use urushi::{Available, Key, resolve};

    use crate::image::{ImageCatalog, RasterCache};
    use crate::math::MathPlan;
    use crate::{Command, Document, PixelSize};

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

    #[test]
    fn body_projects_urushi_image_anchors_when_scrolling() {
        let document = Document::parse("$$\\frac{1}{3}$$".to_owned());
        let catalog = ImageCatalog::load(&document, std::path::Path::new("doc.md"));
        let mut cache = RasterCache::default();
        let layout = Layout::with_graphics(
            &document,
            20,
            10,
            &catalog,
            &mut cache,
            PixelSize {
                width: 8,
                height: 16,
            },
            0,
        );
        let key = Key::of(&MathPlan::element_key(0));
        let mut app = App::default();
        let initial = resolve(&body_view(&app, &layout, 2), Available::size(20, 2))
            .expect("body view should resolve");
        let initial_image = initial
            .anchor(key)
            .expect("math image anchor should resolve");
        assert_eq!(initial_image.y(), 0);
        assert_eq!(
            initial_image
                .visible()
                .expect("image should be visible")
                .y(),
            0
        );

        app.apply(Command::Down, &layout, 2);
        let scrolled = resolve(&body_view(&app, &layout, 2), Available::size(20, 2))
            .expect("scrolled body view should resolve");
        let scrolled_image = scrolled
            .anchor(key)
            .expect("math image anchor should remain");
        assert_eq!(scrolled_image.y(), -1);
        assert_eq!(
            scrolled_image
                .visible()
                .expect("image tail should be visible")
                .y(),
            0
        );
        assert!(scrolled_image.visible().unwrap().height() < scrolled_image.height());
    }
}
