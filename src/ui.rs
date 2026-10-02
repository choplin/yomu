use std::path::Path;

use unicode_width::UnicodeWidthChar;
use urushi::{
    Align, BlockStyle, Color, Key, Length, Overflow, Projection, ProjectionBoundary, StyledText,
    TextAttribute, TextSpan, TextStyle as UrushiTextStyle, VerticalAlign, View, Viewport,
};
use urushi_graphics::{CellSize, Image, ImagePresentation, PixelSize as GraphicsPixelSize};
use urushi_tui_app::SurfaceSize;

use crate::{
    App, CellRect, Layout, LayoutRow, TextStyle,
    theme::{AttributeChoice, ColorChoice, ResolvedTextStyle, Theme},
};

pub(crate) fn view(
    path: &Path,
    app: &App,
    layout: &Layout,
    graphics_status: &str,
    surface: SurfaceSize,
    max_width: Option<u16>,
    theme: &Theme,
) -> View {
    let [header, body, footer] = viewport_areas(surface, max_width);
    let title = format!(" raden — {}", path.display());
    let percent = app.viewport_percent(layout, body.height);
    let status =
        format!(" j/k scroll  d/u page  g/G ends  q quit  {graphics_status}{percent:>5}% ",);
    let status_style = if graphics_status.contains("warning") {
        theme.app.diagnostic
    } else {
        theme.app.footer
    };

    View::column(
        Align::Center,
        [
            fixed_block(
                header.width,
                header.height,
                View::text(
                    title,
                    apply_theme_style(UrushiTextStyle::new(), theme.app.header),
                ),
            ),
            fixed_block(
                body.width,
                body.height,
                body_view(app, layout, body.height, theme),
            ),
            fixed_block(
                footer.width,
                footer.height,
                View::text(
                    status,
                    apply_theme_style(UrushiTextStyle::new(), status_style),
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

fn body_view(app: &App, layout: &Layout, _height: u16, theme: &Theme) -> View {
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
        layout
            .rows()
            .iter()
            .map(|row| layout_row_view(layout, row, theme)),
    );
    View::viewport(
        Viewport::vertical(Projection::new(
            i64::try_from(origin).unwrap_or(i64::MAX),
            ProjectionBoundary::Preserve,
        )),
        document,
    )
}

fn layout_row_view(layout: &Layout, row: &LayoutRow, theme: &Theme) -> View {
    let mut graphics = layout.graphics_for_row(row).collect::<Vec<_>>();
    graphics.sort_by_key(|graphic| graphic.column);
    if graphics.is_empty() {
        return fixed_block(
            u16::try_from(layout.width()).unwrap_or(u16::MAX),
            row.height,
            row_text(row, 0, layout.width(), theme),
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
                row_text(row, column, graphic_column, theme),
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
                .fallback_style(urushi_style(graphic.fallback_style, theme))
                .compose(&image, CellSize::new(graphic.columns, graphic.rows)),
        );
        column = graphic_column.saturating_add(usize::from(graphic.columns));
    }
    if column < layout.width() {
        children.push(fixed_block(
            u16::try_from(layout.width() - column).unwrap_or(u16::MAX),
            row.height,
            row_text(row, column, layout.width(), theme),
        ));
    }

    fixed_block(
        u16::try_from(layout.width()).unwrap_or(u16::MAX),
        row.height,
        View::row(VerticalAlign::Top, children),
    )
}

fn row_text(row: &LayoutRow, start: usize, end: usize, theme: &Theme) -> View {
    let mut spans = Vec::new();
    let mut column = 0usize;
    for fragment in &row.fragments {
        let style = urushi_style(fragment.style, theme);
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

fn urushi_style(source: TextStyle, theme: &Theme) -> UrushiTextStyle {
    let mut style = apply_theme_style(UrushiTextStyle::new(), theme.document.body);
    if source.paragraph {
        style = apply_theme_style(style, theme.document.paragraph);
    }
    if source.quote {
        style = apply_theme_style(style, theme.document.quote.text);
    }
    if source.list_item {
        style = apply_theme_style(style, theme.document.list.item);
    }
    if source.block_code {
        style = apply_theme_style(style, theme.document.code_block.text);
    }
    if source.table {
        style = apply_theme_style(style, theme.document.table.readable_text().cell);
    }
    if source.table_header {
        style = apply_theme_style(style, theme.document.table.readable_text().header);
    }
    if let Some(form) = source.math {
        style = apply_theme_style(
            style,
            match form {
                crate::MathForm::Inline => theme.document.math.inline.readable_text().style,
                crate::MathForm::Display => theme.document.math.display.readable_text().style,
            },
        );
    }
    if source.image {
        style = apply_theme_style(style, theme.document.image.readable_text().style);
    }
    if let Some(kind) = source.html {
        style = apply_theme_style(
            style,
            match kind {
                crate::document::HtmlKind::Inline => theme.document.html.inline,
                crate::document::HtmlKind::Block => theme.document.html.block,
            },
        );
    }
    if let Some(level) = source.heading {
        style = apply_theme_style(
            style,
            theme.document.headings.level(level).readable_text().style,
        );
    }
    if theme.name != "builtin:plain" {
        if let Some(role) = source.syntax_role {
            let color = theme.code_block_palette.get(role);
            style = style.foreground(color.terminal);
        }
        if let Some(color) = source.foreground {
            style = style.foreground(Color::Rgb(color.red, color.green, color.blue));
        }
    }
    if source.emphasis {
        style = apply_theme_style(style, theme.document.emphasis);
    }
    if source.strong {
        style = apply_theme_style(style, theme.document.strong);
    }
    if source.strikethrough {
        style = apply_theme_style(style, theme.document.strikethrough);
    }
    if source.link {
        style = apply_theme_style(style, theme.document.link.label);
    }
    if source.code && !source.block_code {
        style = apply_theme_style(style, theme.document.inline_code.style);
    }
    if source.link_destination {
        style = apply_theme_style(
            style,
            if source.standalone_link_destination {
                theme.document.link.standalone_destination.style
            } else {
                theme.document.link.destination.style
            },
        );
    }
    if source.code_label {
        style = apply_theme_style(style, theme.document.code_block.label.style);
    }
    if source.quote_marker {
        style = apply_theme_style(style, theme.document.quote.marker.style);
    }
    if let Some(marker) = source.list_marker {
        let marker = match marker {
            crate::document::ListMarkerKind::Unordered => theme.document.list.unordered.style,
            crate::document::ListMarkerKind::Ordered => theme.document.list.ordered.style,
            crate::document::ListMarkerKind::TaskChecked => theme.document.list.task_checked.style,
            crate::document::ListMarkerKind::TaskUnchecked => {
                theme.document.list.task_unchecked.style
            }
        };
        style = apply_theme_style(style, marker);
    }
    if source.rule {
        style = apply_theme_style(style, theme.document.rule.style);
    }
    if source.table_border {
        style = apply_theme_style(style, theme.document.table.readable_text().border.style);
    }
    if source.muted {
        style = apply_theme_style(style, theme.app.footer);
    }
    style
}

fn apply_theme_style(mut style: UrushiTextStyle, source: ResolvedTextStyle) -> UrushiTextStyle {
    match source.foreground {
        ColorChoice::Inherit => {}
        ColorChoice::Set(color) => style = style.foreground(color.terminal),
        ColorChoice::Reset => style = style.reset_foreground(),
    }
    match source.background {
        ColorChoice::Inherit => {}
        ColorChoice::Set(color) => style = style.background(color.terminal),
        ColorChoice::Reset => style = style.reset_background(),
    }
    style = apply_attribute(style, source.bold, TextAttribute::Bold);
    style = apply_attribute(style, source.italic, TextAttribute::Italic);
    match source.underline {
        AttributeChoice::Inherit => {}
        AttributeChoice::Enabled => style = style.underlined(),
        AttributeChoice::Disabled => style = style.reset_underline(),
    }
    style = apply_attribute(style, source.strikethrough, TextAttribute::CrossedOut);
    style = apply_attribute(style, source.dim, TextAttribute::Dim);
    style = apply_attribute(style, source.reverse, TextAttribute::Reversed);
    style
}

fn apply_attribute(
    style: UrushiTextStyle,
    choice: AttributeChoice,
    attribute: TextAttribute,
) -> UrushiTextStyle {
    match choice {
        AttributeChoice::Enabled => style.add_attribute(attribute),
        AttributeChoice::Disabled => style.remove_attribute(attribute),
        AttributeChoice::Inherit => style,
    }
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
        let theme = crate::theme::Theme::builtin_dark();
        let initial = resolve(&body_view(&app, &layout, 2, &theme), Available::size(20, 2))
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
        let scrolled = resolve(&body_view(&app, &layout, 2, &theme), Available::size(20, 2))
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

    #[test]
    fn syntax_roles_are_lowered_through_the_selected_theme() {
        let source = TextStyle {
            block_code: true,
            syntax_role: Some(crate::document::SyntaxRole::Variable),
            ..TextStyle::default()
        };
        let dark = crate::theme::resolve(
            "builtin:dark",
            &crate::theme::ThemeSettings::default(),
            None,
            None,
        )
        .expect("dark Theme should resolve");
        let light = crate::theme::resolve(
            "builtin:light",
            &crate::theme::ThemeSettings::default(),
            None,
            None,
        )
        .expect("light Theme should resolve");
        let plain = crate::theme::resolve(
            "builtin:plain",
            &crate::theme::ThemeSettings::default(),
            None,
            None,
        )
        .expect("plain Theme should resolve");

        assert_eq!(
            urushi_style(source, &dark),
            UrushiTextStyle::new().foreground(
                dark.code_block_palette
                    .get(crate::document::SyntaxRole::Variable)
                    .terminal,
            )
        );
        assert_eq!(
            urushi_style(source, &light),
            UrushiTextStyle::new().foreground(
                light
                    .code_block_palette
                    .get(crate::document::SyntaxRole::Variable)
                    .terminal,
            )
        );
        assert_eq!(urushi_style(source, &plain), UrushiTextStyle::new());
    }

    #[test]
    fn code_block_style_owns_plain_text_and_background_while_syntax_owns_token_color() {
        let overrides = toml::from_str::<crate::theme::ThemeSettings>(
            r##"
[palette.code_block]
keyword = "#abcdef"

[styles.document.code_block.text]
foreground = "#123456"
background = "#654321"
"##,
        )
        .expect("code-block Theme should parse");
        let theme = crate::theme::resolve("builtin:dark", &overrides, None, None)
            .expect("code-block Theme should resolve");
        let plain = TextStyle {
            block_code: true,
            ..TextStyle::default()
        };
        let keyword = TextStyle {
            syntax_role: Some(crate::document::SyntaxRole::Keyword),
            ..plain
        };
        let block = apply_theme_style(
            apply_theme_style(UrushiTextStyle::new(), theme.document.body),
            theme.document.code_block.text,
        );
        let keyword_color = theme
            .code_block_palette
            .get(crate::document::SyntaxRole::Keyword);

        assert_eq!(urushi_style(plain, &theme), block);
        assert_eq!(
            urushi_style(keyword, &theme),
            block.foreground(keyword_color.terminal)
        );
    }

    #[test]
    fn inline_code_uses_its_own_semantic_theme_role() {
        let theme = crate::theme::Theme::builtin_dark();
        let source = TextStyle {
            code: true,
            ..TextStyle::default()
        };

        assert_eq!(
            urushi_style(source, &theme),
            apply_theme_style(UrushiTextStyle::new(), theme.document.inline_code.style,)
        );
    }

    #[test]
    fn block_and_inline_roles_compose_in_a_stable_order() {
        let theme = crate::theme::Theme::builtin_dark();
        let source = TextStyle {
            heading: Some(3),
            paragraph: true,
            emphasis: true,
            strong: true,
            link: true,
            ..TextStyle::default()
        };
        let expected = apply_theme_style(
            apply_theme_style(
                apply_theme_style(
                    apply_theme_style(
                        apply_theme_style(UrushiTextStyle::new(), theme.document.body),
                        theme.document.paragraph,
                    ),
                    theme.document.headings.level(3).readable_text().style,
                ),
                theme.document.emphasis,
            ),
            theme.document.strong,
        );
        let expected = apply_theme_style(expected, theme.document.link.label);

        assert_eq!(urushi_style(source, &theme), expected);
    }

    #[test]
    fn display_math_uses_its_distinct_text_appearance() {
        let overrides = toml::from_str::<crate::theme::ThemeSettings>(
            r##"
[styles.document.math.inline]
renderer = "text"

[styles.document.math.inline.text]
foreground = "#112233"

[styles.document.math.display]
renderer = "text"

[styles.document.math.display.text]
foreground = "#aabbcc"
bold = true
"##,
        )
        .expect("math Theme should parse");
        let theme = crate::theme::resolve("builtin:dark", &overrides, None, None)
            .expect("math Theme should resolve");
        let source = TextStyle {
            math: Some(crate::MathForm::Display),
            ..TextStyle::default()
        };

        assert_eq!(
            urushi_style(source, &theme),
            apply_theme_style(
                apply_theme_style(UrushiTextStyle::new(), theme.document.body),
                theme.document.math.display.readable_text().style,
            )
        );
    }

    #[test]
    fn a_later_semantic_role_can_explicitly_clear_an_attribute() {
        let overrides = toml::from_str::<crate::theme::ThemeSettings>(
            r#"
[styles.document.body]
bold = true

[styles.document.heading.h1]
renderer = "text"

[styles.document.heading.h1.text]
foreground = "accent"
bold = false
"#,
        )
        .expect("attribute Theme should parse");
        let theme = crate::theme::resolve("builtin:dark", &overrides, None, None)
            .expect("attribute Theme should resolve");
        let source = TextStyle {
            heading: Some(1),
            ..TextStyle::default()
        };
        let ColorChoice::Set(foreground) = theme
            .document
            .headings
            .level(1)
            .readable_text()
            .style
            .foreground
        else {
            panic!("built-in heading should have a foreground color");
        };

        assert_eq!(
            urushi_style(source, &theme),
            UrushiTextStyle::new().foreground(foreground.terminal)
        );
    }
}
