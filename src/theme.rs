use std::collections::HashSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use unicode_width::UnicodeWidthChar;
use urushi::{Color, ColorScheme, ThemeMode};
use urushi_terminal::TerminalBackground;

use crate::document::SyntaxRole;

// Runtime model. Rendering only sees these validated, contradiction-free values.

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ThemeColor {
    pub(crate) terminal: Color,
    pub(crate) rgb: [u8; 3],
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) enum ColorChoice {
    #[default]
    Inherit,
    Set(ThemeColor),
    Reset,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) enum AttributeChoice {
    #[default]
    Inherit,
    Enabled,
    Disabled,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct ResolvedTextStyle {
    pub(crate) foreground: ColorChoice,
    pub(crate) background: ColorChoice,
    pub(crate) bold: AttributeChoice,
    pub(crate) italic: AttributeChoice,
    pub(crate) underline: AttributeChoice,
    pub(crate) strikethrough: AttributeChoice,
    pub(crate) dim: AttributeChoice,
    pub(crate) reverse: AttributeChoice,
}

#[derive(Clone, Debug)]
pub(crate) struct TextProjection {
    pub(crate) style: ResolvedTextStyle,
    pub(crate) prefix: String,
    pub(crate) suffix: String,
}

#[derive(Clone, Debug)]
pub(crate) struct TextMarker {
    pub(crate) style: ResolvedTextStyle,
    pub(crate) text: String,
}

#[derive(Clone, Debug)]
pub(crate) struct OrderedMarker {
    pub(crate) style: ResolvedTextStyle,
    pub(crate) suffix: String,
}

#[derive(Clone, Debug)]
pub(crate) struct RasterizedHeading {
    pub(crate) foreground: ThemeColor,
    pub(crate) scale: f32,
    pub(crate) underline: bool,
    pub(crate) stroke_radius: u32,
    pub(crate) font_families: Vec<String>,
    pub(crate) fallback: TextProjection,
}

#[derive(Clone, Debug)]
pub(crate) enum HeadingStyle {
    Text(TextProjection),
    Rasterized(RasterizedHeading),
}

impl HeadingStyle {
    pub(crate) const fn readable_text(&self) -> &TextProjection {
        match self {
            Self::Text(text) => text,
            Self::Rasterized(rasterized) => &rasterized.fallback,
        }
    }

    pub(crate) const fn rasterized(&self) -> Option<&RasterizedHeading> {
        match self {
            Self::Text(_) => None,
            Self::Rasterized(rasterized) => Some(rasterized),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RasterizedTable {
    pub(crate) cell: RasterizedTableTextStyle,
    pub(crate) header: RasterizedTableTextStyle,
    pub(crate) border: RasterizedTableBorderStyle,
    pub(crate) font_points: f32,
    pub(crate) horizontal_inset_points: f32,
    pub(crate) vertical_inset_points: f32,
    pub(crate) rule_gap_points: f32,
    pub(crate) fallback: TableTextProjection,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct RasterizedTableTextStyle {
    pub(crate) foreground: ThemeColor,
    pub(crate) background: Option<ThemeColor>,
    pub(crate) bold: bool,
    pub(crate) italic: bool,
    pub(crate) underline: bool,
    pub(crate) strikethrough: bool,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct RasterizedTableBorderStyle {
    pub(crate) color: ThemeColor,
}

#[derive(Clone, Debug)]
pub(crate) enum TableStyle {
    Text(TableTextProjection),
    Rasterized(RasterizedTable),
}

impl TableStyle {
    pub(crate) const fn readable_text(&self) -> &TableTextProjection {
        match self {
            Self::Text(text) => text,
            Self::Rasterized(rasterized) => &rasterized.fallback,
        }
    }

    pub(crate) const fn rasterized(&self) -> Option<&RasterizedTable> {
        match self {
            Self::Text(_) => None,
            Self::Rasterized(rasterized) => Some(rasterized),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RasterizedMath {
    pub(crate) foreground: ThemeColor,
    pub(crate) font_points: f32,
    pub(crate) max_rows: u16,
    pub(crate) fallback: TextProjection,
}

#[derive(Clone, Debug)]
pub(crate) enum MathStyle {
    Text(TextProjection),
    Rasterized(RasterizedMath),
}

impl MathStyle {
    pub(crate) const fn readable_text(&self) -> &TextProjection {
        match self {
            Self::Text(text) => text,
            Self::Rasterized(rasterized) => &rasterized.fallback,
        }
    }

    pub(crate) const fn rasterized(&self) -> Option<&RasterizedMath> {
        match self {
            Self::Text(_) => None,
            Self::Rasterized(rasterized) => Some(rasterized),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RasterizedImage {
    pub(crate) max_rows: u16,
    pub(crate) fallback: TextProjection,
}

#[derive(Clone, Debug)]
pub(crate) enum ImageStyle {
    Text(TextProjection),
    Rasterized(RasterizedImage),
}

impl ImageStyle {
    pub(crate) const fn readable_text(&self) -> &TextProjection {
        match self {
            Self::Text(text) => text,
            Self::Rasterized(rasterized) => &rasterized.fallback,
        }
    }

    pub(crate) const fn rasterized(&self) -> Option<&RasterizedImage> {
        match self {
            Self::Text(_) => None,
            Self::Rasterized(rasterized) => Some(rasterized),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct AppTheme {
    pub(crate) header: ResolvedTextStyle,
    pub(crate) footer: ResolvedTextStyle,
    pub(crate) diagnostic: ResolvedTextStyle,
}

#[derive(Clone, Debug)]
pub(crate) struct HeadingTheme {
    pub(crate) h1: HeadingStyle,
    pub(crate) h2: HeadingStyle,
    pub(crate) h3: HeadingStyle,
    pub(crate) h4: HeadingStyle,
    pub(crate) h5: HeadingStyle,
    pub(crate) h6: HeadingStyle,
}

impl HeadingTheme {
    pub(crate) fn level(&self, level: u8) -> &HeadingStyle {
        match level {
            1 => &self.h1,
            2 => &self.h2,
            3 => &self.h3,
            4 => &self.h4,
            5 => &self.h5,
            _ => &self.h6,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct MathTheme {
    pub(crate) inline: MathStyle,
    pub(crate) display: MathStyle,
}

#[derive(Clone, Debug)]
pub(crate) struct LinkTheme {
    pub(crate) label: ResolvedTextStyle,
    pub(crate) destination: TextProjection,
    pub(crate) standalone_destination: TextProjection,
}

#[derive(Clone, Debug)]
pub(crate) struct CodeBlockTheme {
    pub(crate) text: ResolvedTextStyle,
    pub(crate) label: TextProjection,
}

#[derive(Clone, Debug)]
pub(crate) struct QuoteTheme {
    pub(crate) text: ResolvedTextStyle,
    pub(crate) marker: TextMarker,
}

#[derive(Clone, Debug)]
pub(crate) struct ListTheme {
    pub(crate) item: ResolvedTextStyle,
    pub(crate) unordered: TextMarker,
    pub(crate) ordered: OrderedMarker,
    pub(crate) task_checked: TextMarker,
    pub(crate) task_unchecked: TextMarker,
    pub(crate) indent_cells: u16,
}

#[derive(Clone, Debug)]
pub(crate) struct RuleTheme {
    pub(crate) style: ResolvedTextStyle,
    pub(crate) character: char,
}

#[derive(Clone, Debug)]
pub(crate) struct TableTextProjection {
    pub(crate) cell: ResolvedTextStyle,
    pub(crate) header: ResolvedTextStyle,
    pub(crate) border: TableTextBorder,
}

#[derive(Clone, Debug)]
pub(crate) struct TableTextBorder {
    pub(crate) style: ResolvedTextStyle,
    pub(crate) left: String,
    pub(crate) column: String,
    pub(crate) right: String,
}

#[derive(Clone, Debug)]
pub(crate) struct HtmlTheme {
    pub(crate) inline: ResolvedTextStyle,
    pub(crate) block: ResolvedTextStyle,
}

#[derive(Clone, Debug)]
pub(crate) struct DocumentTheme {
    pub(crate) body: ResolvedTextStyle,
    pub(crate) paragraph: ResolvedTextStyle,
    pub(crate) headings: HeadingTheme,
    pub(crate) emphasis: ResolvedTextStyle,
    pub(crate) strong: ResolvedTextStyle,
    pub(crate) strikethrough: ResolvedTextStyle,
    pub(crate) link: LinkTheme,
    pub(crate) inline_code: TextProjection,
    pub(crate) code_block: CodeBlockTheme,
    pub(crate) quote: QuoteTheme,
    pub(crate) list: ListTheme,
    pub(crate) rule: RuleTheme,
    pub(crate) table: TableStyle,
    pub(crate) math: MathTheme,
    pub(crate) image: ImageStyle,
    pub(crate) html: HtmlTheme,
}

#[derive(Clone, Debug)]
pub(crate) struct Theme {
    pub(crate) name: String,
    pub(crate) identity: [u8; 32],
    pub(crate) app: AppTheme,
    pub(crate) document: DocumentTheme,
    pub(crate) code_block_palette: CodeBlockPalette,
}

// Serializable merge inputs. These mirror the TOML keys and disappear after resolution.

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ThemeSettings {
    palette: PaletteSettings,
    styles: ThemeStyleSettings,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct PaletteSettings {
    text: Option<String>,
    muted: Option<String>,
    accent: Option<String>,
    secondary: Option<String>,
    inline_code: Option<String>,
    surface: Option<String>,
    border: Option<String>,
    error: Option<String>,
    code_block: CodeBlockPaletteSettings,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct CodeBlockPaletteSettings {
    comment: Option<String>,
    variable: Option<String>,
    number: Option<String>,
    type_name: Option<String>,
    string: Option<String>,
    support: Option<String>,
    function: Option<String>,
    keyword: Option<String>,
    deprecated: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ThemeStyleSettings {
    app: AppSettings,
    document: DocumentSettings,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct AppSettings {
    header: TextStyleSettings,
    footer: TextStyleSettings,
    diagnostic: TextStyleSettings,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct DocumentSettings {
    body: TextStyleSettings,
    paragraph: TextStyleSettings,
    heading: HeadingSettings,
    emphasis: TextStyleSettings,
    strong: TextStyleSettings,
    strikethrough: TextStyleSettings,
    link: LinkSettings,
    inline_code: TextSettings,
    code_block: CodeBlockSettings,
    quote: QuoteSettings,
    list: ListSettings,
    rule: RuleSettings,
    table: Option<TableSettings>,
    math: MathSettings,
    image: Option<ImageSettings>,
    html: HtmlSettings,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct HeadingSettings {
    base: TextSettings,
    h1: Option<HeadingLevelSettings>,
    h2: Option<HeadingLevelSettings>,
    h3: Option<HeadingLevelSettings>,
    h4: Option<HeadingLevelSettings>,
    h5: Option<HeadingLevelSettings>,
    h6: Option<HeadingLevelSettings>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct MathSettings {
    inline: Option<MathElementSettings>,
    display: Option<MathElementSettings>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct TextStyleSettings {
    foreground: Option<String>,
    background: Option<String>,
    bold: Option<bool>,
    italic: Option<bool>,
    underline: Option<bool>,
    strikethrough: Option<bool>,
    dim: Option<bool>,
    reverse: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct TextSettings {
    #[serde(flatten)]
    style: TextStyleSettings,
    prefix: Option<String>,
    suffix: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "renderer", rename_all = "snake_case", deny_unknown_fields)]
enum HeadingLevelSettings {
    Text {
        #[serde(default)]
        text: TextSettings,
    },
    Rasterized {
        #[serde(default)]
        text: TextSettings,
        #[serde(default)]
        rasterized: RasterizedHeadingSettings,
    },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "renderer", rename_all = "snake_case", deny_unknown_fields)]
enum TableSettings {
    Text {
        #[serde(default)]
        text: TableTextSettings,
    },
    Rasterized {
        #[serde(default)]
        text: TableTextSettings,
        #[serde(default)]
        rasterized: RasterizedTableSettings,
    },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "renderer", rename_all = "snake_case", deny_unknown_fields)]
enum MathElementSettings {
    Text {
        #[serde(default)]
        text: TextSettings,
    },
    Rasterized {
        #[serde(default)]
        text: TextSettings,
        #[serde(default)]
        rasterized: RasterizedMathSettings,
    },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "renderer", rename_all = "snake_case", deny_unknown_fields)]
enum ImageSettings {
    Text {
        #[serde(default)]
        text: TextSettings,
    },
    Rasterized {
        #[serde(default)]
        text: TextSettings,
        #[serde(default)]
        rasterized: RasterizedImageSettings,
    },
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct TextMarkerSettings {
    #[serde(flatten)]
    style: TextStyleSettings,
    text: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct OrderedMarkerSettings {
    #[serde(flatten)]
    style: TextStyleSettings,
    suffix: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct LinkSettings {
    label: TextStyleSettings,
    destination: TextSettings,
    standalone_destination: TextSettings,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct CodeBlockSettings {
    text: TextStyleSettings,
    label: TextSettings,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct QuoteSettings {
    text: TextStyleSettings,
    marker: TextMarkerSettings,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ListSettings {
    item: TextStyleSettings,
    unordered: TextMarkerSettings,
    ordered: OrderedMarkerSettings,
    task_checked: TextMarkerSettings,
    task_unchecked: TextMarkerSettings,
    indent_cells: Option<u16>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RuleSettings {
    #[serde(flatten)]
    style: TextStyleSettings,
    character: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct TableTextSettings {
    cell: TextStyleSettings,
    header: TextStyleSettings,
    border: TableTextBorderSettings,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct TableTextBorderSettings {
    #[serde(flatten)]
    style: TextStyleSettings,
    left: Option<String>,
    column: Option<String>,
    right: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct HtmlSettings {
    inline: TextStyleSettings,
    block: TextStyleSettings,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RasterizedHeadingSettings {
    foreground: Option<String>,
    scale: Option<f32>,
    underline: Option<bool>,
    stroke_radius: Option<u32>,
    font_families: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RasterizedTableSettings {
    cell: RasterizedTableTextSettings,
    header: RasterizedTableTextSettings,
    border: RasterizedTableBorderSettings,
    font_points: Option<f32>,
    horizontal_inset_points: Option<f32>,
    vertical_inset_points: Option<f32>,
    rule_gap_points: Option<f32>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RasterizedTableTextSettings {
    foreground: Option<String>,
    background: Option<String>,
    bold: Option<bool>,
    italic: Option<bool>,
    underline: Option<bool>,
    strikethrough: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RasterizedTableBorderSettings {
    color: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RasterizedMathSettings {
    foreground: Option<String>,
    font_points: Option<f32>,
    max_rows: Option<u16>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RasterizedImageSettings {
    max_rows: Option<u16>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ThemeFile {
    #[serde(default)]
    extends: Option<String>,
    #[serde(flatten)]
    theme: ThemeSettings,
}

// Loading and resolution: merge settings first, then construct one runtime Theme.

pub(crate) fn resolve(
    selected: &str,
    overrides: &ThemeSettings,
    theme_directory: Option<&Path>,
    background: Option<TerminalBackground>,
) -> Result<Theme> {
    let selected = if selected == "builtin:auto" {
        match (ThemeMode::Auto {
            fallback: ColorScheme::Dark,
        })
        .resolve(background)
        {
            ColorScheme::Light => "builtin:light",
            ColorScheme::Dark => "builtin:dark",
        }
    } else {
        selected
    };
    let mut merged = default_settings();
    let mut chain = Vec::new();
    load_chain(selected, theme_directory, &mut HashSet::new(), &mut chain)?;
    for layer in chain {
        merged.merge(layer);
    }
    merged.merge(overrides.clone());
    Theme::from_settings(selected, &merged)
}

pub(crate) fn needs_terminal_background(selected: &str) -> bool {
    selected == "builtin:auto"
}

fn load_chain(
    selected: &str,
    theme_directory: Option<&Path>,
    visiting: &mut HashSet<String>,
    output: &mut Vec<ThemeSettings>,
) -> Result<()> {
    if !visiting.insert(selected.to_owned()) {
        bail!("theme inheritance cycle includes {selected:?}");
    }
    if let Some(theme) = builtin(selected) {
        output.push(theme);
    } else {
        validate_theme_name(selected)?;
        let directory = theme_directory.ok_or_else(|| {
            anyhow!("cannot load user theme {selected:?} because no config directory is available")
        })?;
        let path = directory.join(format!("{selected}.toml"));
        let source = fs::read_to_string(&path)
            .with_context(|| format!("could not read theme file {}", path.display()))?;
        let file = toml::from_str::<ThemeFile>(&source)
            .with_context(|| format!("could not parse theme file {}", path.display()))?;
        if let Some(base) = file.extends.as_deref() {
            if base == "builtin:auto" {
                bail!(
                    "user themes cannot extend builtin:auto; extend builtin:light or builtin:dark"
                );
            }
            load_chain(base, theme_directory, visiting, output)?;
        }
        output.push(file.theme);
    }
    visiting.remove(selected);
    Ok(())
}

fn validate_theme_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("invalid theme name {name:?}; use ASCII letters, digits, '-' or '_'");
    }
    Ok(())
}

impl Theme {
    pub(crate) fn builtin_dark() -> Self {
        resolve("builtin:dark", &ThemeSettings::default(), None, None)
            .expect("built-in dark Theme must remain valid")
    }

    fn from_settings(name: &str, theme: &ThemeSettings) -> Result<Self> {
        let palette = Palette::from_settings(&theme.palette)?;
        let app = AppTheme {
            header: resolve_text_style(&theme.styles.app.header, &palette, "styles.app.header")?,
            footer: resolve_text_style(&theme.styles.app.footer, &palette, "styles.app.footer")?,
            diagnostic: resolve_text_style(
                &theme.styles.app.diagnostic,
                &palette,
                "styles.app.diagnostic",
            )?,
        };
        let document = &theme.styles.document;
        let document = DocumentTheme {
            body: resolve_text_style(&document.body, &palette, "styles.document.body")?,
            paragraph: resolve_text_style(
                &document.paragraph,
                &palette,
                "styles.document.paragraph",
            )?,
            headings: resolve_headings(&document.heading, &palette)?,
            emphasis: resolve_text_style(&document.emphasis, &palette, "styles.document.emphasis")?,
            strong: resolve_text_style(&document.strong, &palette, "styles.document.strong")?,
            strikethrough: resolve_text_style(
                &document.strikethrough,
                &palette,
                "styles.document.strikethrough",
            )?,
            link: LinkTheme {
                label: resolve_text_style(
                    &document.link.label,
                    &palette,
                    "styles.document.link.label",
                )?,
                destination: resolve_text(
                    &document.link.destination,
                    &palette,
                    "styles.document.link.destination",
                )?,
                standalone_destination: resolve_text(
                    &document.link.standalone_destination,
                    &palette,
                    "styles.document.link.standalone_destination",
                )?,
            },
            inline_code: resolve_text(
                &document.inline_code,
                &palette,
                "styles.document.inline_code",
            )?,
            code_block: CodeBlockTheme {
                text: resolve_text_style(
                    &document.code_block.text,
                    &palette,
                    "styles.document.code_block.text",
                )?,
                label: resolve_text(
                    &document.code_block.label,
                    &palette,
                    "styles.document.code_block.label",
                )?,
            },
            quote: QuoteTheme {
                text: resolve_text_style(
                    &document.quote.text,
                    &palette,
                    "styles.document.quote.text",
                )?,
                marker: resolve_text_marker(
                    &document.quote.marker,
                    &palette,
                    "styles.document.quote.marker",
                )?,
            },
            list: resolve_list(&document.list, &palette)?,
            rule: resolve_rule(&document.rule, &palette)?,
            table: resolve_table(&document.table, &palette)?,
            math: MathTheme {
                inline: resolve_math(&document.math.inline, &palette, "inline")?,
                display: resolve_math(&document.math.display, &palette, "display")?,
            },
            image: resolve_image(&document.image, &palette)?,
            html: HtmlTheme {
                inline: resolve_text_style(
                    &document.html.inline,
                    &palette,
                    "styles.document.html.inline",
                )?,
                block: resolve_text_style(
                    &document.html.block,
                    &palette,
                    "styles.document.html.block",
                )?,
            },
        };
        let identity = *blake3::hash(format!("{name}\n{theme:#?}").as_bytes()).as_bytes();
        Ok(Self {
            name: name.to_owned(),
            identity,
            app,
            document,
            code_block_palette: palette.code_block,
        })
    }
}

#[derive(Clone, Copy)]
struct Palette {
    text: ThemeColor,
    muted: ThemeColor,
    accent: ThemeColor,
    secondary: ThemeColor,
    inline_code: ThemeColor,
    surface: ThemeColor,
    border: ThemeColor,
    error: ThemeColor,
    code_block: CodeBlockPalette,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct CodeBlockPalette {
    comment: ThemeColor,
    variable: ThemeColor,
    number: ThemeColor,
    type_name: ThemeColor,
    string: ThemeColor,
    support: ThemeColor,
    function: ThemeColor,
    keyword: ThemeColor,
    deprecated: ThemeColor,
}

impl CodeBlockPalette {
    pub(crate) const fn get(self, role: SyntaxRole) -> ThemeColor {
        match role {
            SyntaxRole::Comment => self.comment,
            SyntaxRole::Variable => self.variable,
            SyntaxRole::Number => self.number,
            SyntaxRole::TypeName => self.type_name,
            SyntaxRole::String => self.string,
            SyntaxRole::Support => self.support,
            SyntaxRole::Function => self.function,
            SyntaxRole::Keyword => self.keyword,
            SyntaxRole::Deprecated => self.deprecated,
        }
    }
}

impl Palette {
    fn from_settings(source: &PaletteSettings) -> Result<Self> {
        Ok(Self {
            text: required_color(&source.text, "palette.text")?,
            muted: required_color(&source.muted, "palette.muted")?,
            accent: required_color(&source.accent, "palette.accent")?,
            secondary: required_color(&source.secondary, "palette.secondary")?,
            inline_code: required_color(&source.inline_code, "palette.inline_code")?,
            surface: required_color(&source.surface, "palette.surface")?,
            border: required_color(&source.border, "palette.border")?,
            error: required_color(&source.error, "palette.error")?,
            code_block: resolve_code_block_palette(&source.code_block)?,
        })
    }

    fn get(self, name: &str) -> Option<ThemeColor> {
        match name {
            "text" => Some(self.text),
            "muted" => Some(self.muted),
            "accent" => Some(self.accent),
            "secondary" => Some(self.secondary),
            "inline_code" => Some(self.inline_code),
            "surface" => Some(self.surface),
            "border" => Some(self.border),
            "error" => Some(self.error),
            _ => None,
        }
    }
}

fn required_color(value: &Option<String>, path: &str) -> Result<ThemeColor> {
    let value = value
        .as_deref()
        .ok_or_else(|| anyhow!("missing required theme value {path}"))?;
    parse_literal_color(value).with_context(|| format!("invalid color at {path}"))
}

fn resolve_code_block_palette(source: &CodeBlockPaletteSettings) -> Result<CodeBlockPalette> {
    Ok(CodeBlockPalette {
        comment: required_color(&source.comment, "palette.code_block.comment")?,
        variable: required_color(&source.variable, "palette.code_block.variable")?,
        number: required_color(&source.number, "palette.code_block.number")?,
        type_name: required_color(&source.type_name, "palette.code_block.type_name")?,
        string: required_color(&source.string, "palette.code_block.string")?,
        support: required_color(&source.support, "palette.code_block.support")?,
        function: required_color(&source.function, "palette.code_block.function")?,
        keyword: required_color(&source.keyword, "palette.code_block.keyword")?,
        deprecated: required_color(&source.deprecated, "palette.code_block.deprecated")?,
    })
}

fn resolve_color(value: &str, palette: Palette, path: &str) -> Result<ThemeColor> {
    if let Some(color) = palette.get(value) {
        return Ok(color);
    }
    parse_literal_color(value)
        .with_context(|| format!("invalid color or palette reference {value:?} at {path}"))
}

fn parse_literal_color(value: &str) -> Result<ThemeColor> {
    let (terminal, rgb) = match value {
        "black" => (Color::BLACK, [0, 0, 0]),
        "red" => (Color::RED, [205, 49, 49]),
        "green" => (Color::GREEN, [13, 188, 121]),
        "yellow" => (Color::YELLOW, [229, 229, 16]),
        "blue" => (Color::BLUE, [36, 114, 200]),
        "magenta" => (Color::MAGENTA, [188, 63, 188]),
        "cyan" => (Color::CYAN, [17, 168, 205]),
        "white" => (Color::WHITE, [229, 229, 229]),
        "bright_black" => (Color::BRIGHT_BLACK, [102, 102, 102]),
        "bright_red" => (Color::BRIGHT_RED, [241, 76, 76]),
        "bright_green" => (Color::BRIGHT_GREEN, [35, 209, 139]),
        "bright_yellow" => (Color::BRIGHT_YELLOW, [245, 245, 67]),
        "bright_blue" => (Color::BRIGHT_BLUE, [59, 142, 234]),
        "bright_magenta" => (Color::BRIGHT_MAGENTA, [214, 112, 214]),
        "bright_cyan" => (Color::BRIGHT_CYAN, [41, 184, 219]),
        "bright_white" => (Color::BRIGHT_WHITE, [255, 255, 255]),
        value if value.starts_with('#') && value.len() == 7 => {
            let red = u8::from_str_radix(&value[1..3], 16)?;
            let green = u8::from_str_radix(&value[3..5], 16)?;
            let blue = u8::from_str_radix(&value[5..7], 16)?;
            (Color::Rgb(red, green, blue), [red, green, blue])
        }
        _ => bail!("expected #rrggbb or a supported ANSI color name"),
    };
    Ok(ThemeColor { terminal, rgb })
}

fn resolve_text_style(
    source: &TextStyleSettings,
    palette: &Palette,
    path: &str,
) -> Result<ResolvedTextStyle> {
    Ok(ResolvedTextStyle {
        foreground: resolve_color_choice(
            source.foreground.as_deref(),
            *palette,
            &format!("{path}.foreground"),
        )?,
        background: resolve_color_choice(
            source.background.as_deref(),
            *palette,
            &format!("{path}.background"),
        )?,
        bold: attribute_choice(source.bold),
        italic: attribute_choice(source.italic),
        underline: attribute_choice(source.underline),
        strikethrough: attribute_choice(source.strikethrough),
        dim: attribute_choice(source.dim),
        reverse: attribute_choice(source.reverse),
    })
}

fn resolve_color_choice(value: Option<&str>, palette: Palette, path: &str) -> Result<ColorChoice> {
    match value {
        None => Ok(ColorChoice::Inherit),
        Some("none") => Ok(ColorChoice::Reset),
        Some(value) => Ok(ColorChoice::Set(resolve_color(value, palette, path)?)),
    }
}

const fn attribute_choice(value: Option<bool>) -> AttributeChoice {
    match value {
        None => AttributeChoice::Inherit,
        Some(true) => AttributeChoice::Enabled,
        Some(false) => AttributeChoice::Disabled,
    }
}

fn resolve_text(source: &TextSettings, palette: &Palette, path: &str) -> Result<TextProjection> {
    Ok(TextProjection {
        style: resolve_text_style(&source.style, palette, path)?,
        prefix: source.prefix.clone().unwrap_or_default(),
        suffix: source.suffix.clone().unwrap_or_default(),
    })
}

fn resolve_headings(source: &HeadingSettings, palette: &Palette) -> Result<HeadingTheme> {
    Ok(HeadingTheme {
        h1: resolve_heading(&source.base, &source.h1, palette, "h1")?,
        h2: resolve_heading(&source.base, &source.h2, palette, "h2")?,
        h3: resolve_heading(&source.base, &source.h3, palette, "h3")?,
        h4: resolve_heading(&source.base, &source.h4, palette, "h4")?,
        h5: resolve_heading(&source.base, &source.h5, palette, "h5")?,
        h6: resolve_heading(&source.base, &source.h6, palette, "h6")?,
    })
}

fn resolve_heading(
    base: &TextSettings,
    source: &Option<HeadingLevelSettings>,
    palette: &Palette,
    level: &str,
) -> Result<HeadingStyle> {
    let path = format!("styles.document.heading.{level}");
    let source = source
        .as_ref()
        .ok_or_else(|| anyhow!("missing {path}.renderer"))?;
    let (rasterized, text_settings) = match source {
        HeadingLevelSettings::Text { text: settings } => {
            let mut text = base.clone();
            text.merge(settings.clone());
            return Ok(HeadingStyle::Text(resolve_text(
                &text,
                palette,
                &format!("{path}.text"),
            )?));
        }
        HeadingLevelSettings::Rasterized { text, rasterized } => (rasterized, text),
    };
    let mut text = base.clone();
    text.merge(text_settings.clone());
    let text = resolve_text(&text, palette, &format!("{path}.text"))?;
    let scale = rasterized
        .scale
        .ok_or_else(|| anyhow!("missing {path}.rasterized.scale"))?;
    if !(0.5..=8.0).contains(&scale) {
        bail!("{path}.rasterized.scale must be between 0.5 and 8.0");
    }
    let stroke_radius = rasterized.stroke_radius.unwrap_or(0);
    if stroke_radius > 8 {
        bail!("{path}.rasterized.stroke_radius must be at most 8");
    }
    Ok(HeadingStyle::Rasterized(RasterizedHeading {
        foreground: required_raster_color(
            &rasterized.foreground,
            palette,
            &format!("{path}.rasterized.foreground"),
        )?,
        scale,
        underline: rasterized.underline.unwrap_or(false),
        stroke_radius,
        font_families: rasterized.font_families.clone().unwrap_or_default(),
        fallback: text,
    }))
}

fn resolve_table(source: &Option<TableSettings>, palette: &Palette) -> Result<TableStyle> {
    let path = "styles.document.table";
    let source = source
        .as_ref()
        .ok_or_else(|| anyhow!("missing {path}.renderer"))?;
    let (raster, text) = match source {
        TableSettings::Text { text } => {
            return Ok(TableStyle::Text(resolve_table_text(
                text,
                palette,
                &format!("{path}.text"),
            )?));
        }
        TableSettings::Rasterized { text, rasterized } => (rasterized, text),
    };
    let text = resolve_table_text(text, palette, &format!("{path}.text"))?;
    let cell =
        resolve_rasterized_table_text(&raster.cell, palette, &format!("{path}.rasterized.cell"))?;
    let mut header = raster.cell.clone();
    header.merge(raster.header.clone());
    let header =
        resolve_rasterized_table_text(&header, palette, &format!("{path}.rasterized.header"))?;
    Ok(TableStyle::Rasterized(RasterizedTable {
        cell,
        header,
        border: RasterizedTableBorderStyle {
            color: required_raster_color(
                &raster.border.color,
                palette,
                &format!("{path}.rasterized.border.color"),
            )?,
        },
        font_points: positive(
            raster.font_points,
            &format!("{path}.rasterized.font_points"),
        )?,
        horizontal_inset_points: nonnegative(
            raster.horizontal_inset_points,
            &format!("{path}.rasterized.horizontal_inset_points"),
        )?,
        vertical_inset_points: nonnegative(
            raster.vertical_inset_points,
            &format!("{path}.rasterized.vertical_inset_points"),
        )?,
        rule_gap_points: nonnegative(
            raster.rule_gap_points,
            &format!("{path}.rasterized.rule_gap_points"),
        )?,
        fallback: text,
    }))
}

fn resolve_rasterized_table_text(
    source: &RasterizedTableTextSettings,
    palette: &Palette,
    path: &str,
) -> Result<RasterizedTableTextStyle> {
    let background = source
        .background
        .as_deref()
        .ok_or_else(|| anyhow!("missing {path}.background"))?;
    Ok(RasterizedTableTextStyle {
        foreground: required_raster_color(
            &source.foreground,
            palette,
            &format!("{path}.foreground"),
        )?,
        background: if background == "none" {
            None
        } else {
            Some(resolve_color(
                background,
                *palette,
                &format!("{path}.background"),
            )?)
        },
        bold: source.bold.ok_or_else(|| anyhow!("missing {path}.bold"))?,
        italic: source
            .italic
            .ok_or_else(|| anyhow!("missing {path}.italic"))?,
        underline: source
            .underline
            .ok_or_else(|| anyhow!("missing {path}.underline"))?,
        strikethrough: source
            .strikethrough
            .ok_or_else(|| anyhow!("missing {path}.strikethrough"))?,
    })
}

fn resolve_table_text(
    source: &TableTextSettings,
    palette: &Palette,
    path: &str,
) -> Result<TableTextProjection> {
    Ok(TableTextProjection {
        cell: resolve_text_style(&source.cell, palette, &format!("{path}.cell"))?,
        header: resolve_text_style(&source.header, palette, &format!("{path}.header"))?,
        border: TableTextBorder {
            style: resolve_text_style(&source.border.style, palette, &format!("{path}.border"))?,
            left: required_token(&source.border.left, &format!("{path}.border.left"))?,
            column: required_token(&source.border.column, &format!("{path}.border.column"))?,
            right: required_token(&source.border.right, &format!("{path}.border.right"))?,
        },
    })
}

fn resolve_math(
    source: &Option<MathElementSettings>,
    palette: &Palette,
    form: &str,
) -> Result<MathStyle> {
    let path = format!("styles.document.math.{form}");
    let source = source
        .as_ref()
        .ok_or_else(|| anyhow!("missing {path}.renderer"))?;
    let (rasterized, text) = match source {
        MathElementSettings::Text { text } => {
            return Ok(MathStyle::Text(resolve_text(
                text,
                palette,
                &format!("{path}.text"),
            )?));
        }
        MathElementSettings::Rasterized { text, rasterized } => (rasterized, text),
    };
    let text = resolve_text(text, palette, &format!("{path}.text"))?;
    let max_rows = rasterized
        .max_rows
        .ok_or_else(|| anyhow!("missing {path}.rasterized.max_rows"))?;
    if max_rows == 0 || max_rows > 256 {
        bail!("{path}.rasterized.max_rows must be between 1 and 256");
    }
    Ok(MathStyle::Rasterized(RasterizedMath {
        foreground: required_raster_color(
            &rasterized.foreground,
            palette,
            &format!("{path}.rasterized.foreground"),
        )?,
        font_points: positive(
            rasterized.font_points,
            &format!("{path}.rasterized.font_points"),
        )?,
        max_rows,
        fallback: text,
    }))
}

fn resolve_image(source: &Option<ImageSettings>, palette: &Palette) -> Result<ImageStyle> {
    let path = "styles.document.image";
    let source = source
        .as_ref()
        .ok_or_else(|| anyhow!("missing {path}.renderer"))?;
    let (rasterized, text) = match source {
        ImageSettings::Text { text } => {
            return Ok(ImageStyle::Text(resolve_text(
                text,
                palette,
                &format!("{path}.text"),
            )?));
        }
        ImageSettings::Rasterized { text, rasterized } => (rasterized, text),
    };
    let text = resolve_text(text, palette, &format!("{path}.text"))?;
    let max_rows = rasterized
        .max_rows
        .ok_or_else(|| anyhow!("missing {path}.rasterized.max_rows"))?;
    if max_rows == 0 || max_rows > 256 {
        bail!("{path}.rasterized.max_rows must be between 1 and 256");
    }
    Ok(ImageStyle::Rasterized(RasterizedImage {
        max_rows,
        fallback: text,
    }))
}

fn resolve_text_marker(
    source: &TextMarkerSettings,
    palette: &Palette,
    path: &str,
) -> Result<TextMarker> {
    Ok(TextMarker {
        style: resolve_text_style(&source.style, palette, path)?,
        text: required_token(&source.text, &format!("{path}.text"))?,
    })
}

fn resolve_ordered_marker(
    source: &OrderedMarkerSettings,
    palette: &Palette,
    path: &str,
) -> Result<OrderedMarker> {
    Ok(OrderedMarker {
        style: resolve_text_style(&source.style, palette, path)?,
        suffix: required_token(&source.suffix, &format!("{path}.suffix"))?,
    })
}

fn resolve_list(source: &ListSettings, palette: &Palette) -> Result<ListTheme> {
    let path = "styles.document.list";
    let indent_cells = source
        .indent_cells
        .ok_or_else(|| anyhow!("missing {path}.indent_cells"))?;
    if indent_cells > 16 {
        bail!("{path}.indent_cells must be at most 16");
    }
    Ok(ListTheme {
        item: resolve_text_style(&source.item, palette, &format!("{path}.item"))?,
        unordered: resolve_text_marker(&source.unordered, palette, &format!("{path}.unordered"))?,
        ordered: resolve_ordered_marker(&source.ordered, palette, &format!("{path}.ordered"))?,
        task_checked: resolve_text_marker(
            &source.task_checked,
            palette,
            &format!("{path}.task_checked"),
        )?,
        task_unchecked: resolve_text_marker(
            &source.task_unchecked,
            palette,
            &format!("{path}.task_unchecked"),
        )?,
        indent_cells,
    })
}

fn resolve_rule(source: &RuleSettings, palette: &Palette) -> Result<RuleTheme> {
    let path = "styles.document.rule";
    let value = required_token(&source.character, &format!("{path}.character"))?;
    let mut characters = value.chars();
    let character = characters
        .next()
        .ok_or_else(|| anyhow!("{path}.character cannot be empty"))?;
    if characters.next().is_some() || character.width().unwrap_or(0) != 1 {
        bail!("{path}.character must be exactly one terminal cell");
    }
    Ok(RuleTheme {
        style: resolve_text_style(&source.style, palette, path)?,
        character,
    })
}

fn required_token(source: &Option<String>, path: &str) -> Result<String> {
    let value = source
        .clone()
        .ok_or_else(|| anyhow!("missing required theme value {path}"))?;
    if value.contains(['\n', '\r']) {
        bail!("{path} must stay on one terminal row");
    }
    Ok(value)
}

fn required_raster_color(
    source: &Option<String>,
    palette: &Palette,
    path: &str,
) -> Result<ThemeColor> {
    let value = source.as_deref().ok_or_else(|| anyhow!("missing {path}"))?;
    if value == "none" {
        bail!("{path} cannot be 'none'");
    }
    resolve_color(value, *palette, path)
}

fn positive(value: Option<f32>, path: &str) -> Result<f32> {
    let value = value.ok_or_else(|| anyhow!("missing {path}"))?;
    if !value.is_finite() || value <= 0.0 || value > 128.0 {
        bail!("{path} must be finite and between 0 and 128");
    }
    Ok(value)
}

fn nonnegative(value: Option<f32>, path: &str) -> Result<f32> {
    let value = value.ok_or_else(|| anyhow!("missing {path}"))?;
    if !value.is_finite() || !(0.0..=128.0).contains(&value) {
        bail!("{path} must be finite and between 0 and 128");
    }
    Ok(value)
}

// Recursive merge behavior for the serializable settings layer.

trait Merge {
    fn merge(&mut self, overlay: Self);
}

macro_rules! replace_options {
    ($target:expr, $overlay:expr, $($field:ident),+ $(,)?) => {
        $(if $overlay.$field.is_some() { $target.$field = $overlay.$field; })+
    };
}

impl Merge for ThemeSettings {
    fn merge(&mut self, overlay: Self) {
        self.palette.merge(overlay.palette);
        self.styles.merge(overlay.styles);
    }
}

impl Merge for PaletteSettings {
    fn merge(&mut self, overlay: Self) {
        replace_options!(
            self,
            overlay,
            text,
            muted,
            accent,
            secondary,
            inline_code,
            surface,
            border,
            error
        );
        self.code_block.merge(overlay.code_block);
    }
}

impl Merge for CodeBlockPaletteSettings {
    fn merge(&mut self, overlay: Self) {
        replace_options!(
            self, overlay, comment, variable, number, type_name, string, support, function,
            keyword, deprecated
        );
    }
}

impl Merge for ThemeStyleSettings {
    fn merge(&mut self, overlay: Self) {
        self.app.merge(overlay.app);
        self.document.merge(overlay.document);
    }
}

impl Merge for AppSettings {
    fn merge(&mut self, overlay: Self) {
        self.header.merge(overlay.header);
        self.footer.merge(overlay.footer);
        self.diagnostic.merge(overlay.diagnostic);
    }
}

impl Merge for DocumentSettings {
    fn merge(&mut self, overlay: Self) {
        self.body.merge(overlay.body);
        self.paragraph.merge(overlay.paragraph);
        self.heading.merge(overlay.heading);
        self.emphasis.merge(overlay.emphasis);
        self.strong.merge(overlay.strong);
        self.strikethrough.merge(overlay.strikethrough);
        self.link.merge(overlay.link);
        self.inline_code.merge(overlay.inline_code);
        self.code_block.merge(overlay.code_block);
        self.quote.merge(overlay.quote);
        self.list.merge(overlay.list);
        self.rule.merge(overlay.rule);
        merge_optional(&mut self.table, overlay.table);
        self.math.merge(overlay.math);
        merge_optional(&mut self.image, overlay.image);
        self.html.merge(overlay.html);
    }
}

impl Merge for HeadingSettings {
    fn merge(&mut self, overlay: Self) {
        self.base.merge(overlay.base);
        merge_optional(&mut self.h1, overlay.h1);
        merge_optional(&mut self.h2, overlay.h2);
        merge_optional(&mut self.h3, overlay.h3);
        merge_optional(&mut self.h4, overlay.h4);
        merge_optional(&mut self.h5, overlay.h5);
        merge_optional(&mut self.h6, overlay.h6);
    }
}

impl Merge for MathSettings {
    fn merge(&mut self, overlay: Self) {
        merge_optional(&mut self.inline, overlay.inline);
        merge_optional(&mut self.display, overlay.display);
    }
}

fn merge_optional<T: Merge>(target: &mut Option<T>, overlay: Option<T>) {
    let Some(overlay) = overlay else {
        return;
    };
    if let Some(target) = target.as_mut() {
        target.merge(overlay);
    } else {
        *target = Some(overlay);
    }
}

impl Merge for HeadingLevelSettings {
    fn merge(&mut self, overlay: Self) {
        match (self, overlay) {
            (Self::Text { text }, Self::Text { text: overlay }) => text.merge(overlay),
            (
                Self::Rasterized { text, rasterized },
                Self::Rasterized {
                    text: overlay_text,
                    rasterized: overlay_rasterized,
                },
            ) => {
                text.merge(overlay_text);
                rasterized.merge(overlay_rasterized);
            }
            (target, overlay) => *target = overlay,
        }
    }
}

impl Merge for TableSettings {
    fn merge(&mut self, overlay: Self) {
        match (self, overlay) {
            (Self::Text { text }, Self::Text { text: overlay }) => text.merge(overlay),
            (
                Self::Rasterized { text, rasterized },
                Self::Rasterized {
                    text: overlay_text,
                    rasterized: overlay_rasterized,
                },
            ) => {
                text.merge(overlay_text);
                rasterized.merge(overlay_rasterized);
            }
            (target, overlay) => *target = overlay,
        }
    }
}

impl Merge for MathElementSettings {
    fn merge(&mut self, overlay: Self) {
        match (self, overlay) {
            (Self::Text { text }, Self::Text { text: overlay }) => text.merge(overlay),
            (
                Self::Rasterized { text, rasterized },
                Self::Rasterized {
                    text: overlay_text,
                    rasterized: overlay_rasterized,
                },
            ) => {
                text.merge(overlay_text);
                rasterized.merge(overlay_rasterized);
            }
            (target, overlay) => *target = overlay,
        }
    }
}

impl Merge for ImageSettings {
    fn merge(&mut self, overlay: Self) {
        match (self, overlay) {
            (Self::Text { text }, Self::Text { text: overlay }) => text.merge(overlay),
            (
                Self::Rasterized { text, rasterized },
                Self::Rasterized {
                    text: overlay_text,
                    rasterized: overlay_rasterized,
                },
            ) => {
                text.merge(overlay_text);
                rasterized.merge(overlay_rasterized);
            }
            (target, overlay) => *target = overlay,
        }
    }
}

impl Merge for TextStyleSettings {
    fn merge(&mut self, overlay: Self) {
        replace_options!(
            self,
            overlay,
            foreground,
            background,
            bold,
            italic,
            underline,
            strikethrough,
            dim,
            reverse
        );
    }
}

impl Merge for TextSettings {
    fn merge(&mut self, overlay: Self) {
        self.style.merge(overlay.style);
        replace_options!(self, overlay, prefix, suffix);
    }
}

impl Merge for TextMarkerSettings {
    fn merge(&mut self, overlay: Self) {
        self.style.merge(overlay.style);
        replace_options!(self, overlay, text);
    }
}

impl Merge for OrderedMarkerSettings {
    fn merge(&mut self, overlay: Self) {
        self.style.merge(overlay.style);
        replace_options!(self, overlay, suffix);
    }
}

impl Merge for LinkSettings {
    fn merge(&mut self, overlay: Self) {
        self.label.merge(overlay.label);
        self.destination.merge(overlay.destination);
        self.standalone_destination
            .merge(overlay.standalone_destination);
    }
}

impl Merge for CodeBlockSettings {
    fn merge(&mut self, overlay: Self) {
        self.text.merge(overlay.text);
        self.label.merge(overlay.label);
    }
}

impl Merge for QuoteSettings {
    fn merge(&mut self, overlay: Self) {
        self.text.merge(overlay.text);
        self.marker.merge(overlay.marker);
    }
}

impl Merge for ListSettings {
    fn merge(&mut self, overlay: Self) {
        self.item.merge(overlay.item);
        self.unordered.merge(overlay.unordered);
        self.ordered.merge(overlay.ordered);
        self.task_checked.merge(overlay.task_checked);
        self.task_unchecked.merge(overlay.task_unchecked);
        replace_options!(self, overlay, indent_cells);
    }
}

impl Merge for RuleSettings {
    fn merge(&mut self, overlay: Self) {
        self.style.merge(overlay.style);
        replace_options!(self, overlay, character);
    }
}

impl Merge for TableTextSettings {
    fn merge(&mut self, overlay: Self) {
        self.cell.merge(overlay.cell);
        self.header.merge(overlay.header);
        self.border.merge(overlay.border);
    }
}

impl Merge for TableTextBorderSettings {
    fn merge(&mut self, overlay: Self) {
        self.style.merge(overlay.style);
        replace_options!(self, overlay, left, column, right);
    }
}

impl Merge for HtmlSettings {
    fn merge(&mut self, overlay: Self) {
        self.inline.merge(overlay.inline);
        self.block.merge(overlay.block);
    }
}

impl Merge for RasterizedHeadingSettings {
    fn merge(&mut self, overlay: Self) {
        replace_options!(
            self,
            overlay,
            foreground,
            scale,
            underline,
            stroke_radius,
            font_families
        );
    }
}

impl Merge for RasterizedTableSettings {
    fn merge(&mut self, overlay: Self) {
        self.cell.merge(overlay.cell);
        self.header.merge(overlay.header);
        self.border.merge(overlay.border);
        replace_options!(
            self,
            overlay,
            font_points,
            horizontal_inset_points,
            vertical_inset_points,
            rule_gap_points
        );
    }
}

impl Merge for RasterizedTableTextSettings {
    fn merge(&mut self, overlay: Self) {
        replace_options!(
            self,
            overlay,
            foreground,
            background,
            bold,
            italic,
            underline,
            strikethrough
        );
    }
}

impl Merge for RasterizedTableBorderSettings {
    fn merge(&mut self, overlay: Self) {
        replace_options!(self, overlay, color);
    }
}

impl Merge for RasterizedMathSettings {
    fn merge(&mut self, overlay: Self) {
        replace_options!(self, overlay, foreground, font_points, max_rows);
    }
}

impl Merge for RasterizedImageSettings {
    fn merge(&mut self, overlay: Self) {
        replace_options!(self, overlay, max_rows);
    }
}

fn default_settings() -> ThemeSettings {
    builtin_dark()
}

fn builtin(name: &str) -> Option<ThemeSettings> {
    match name {
        "builtin:dark" => Some(ThemeSettings::default()),
        "builtin:light" => Some(builtin_light()),
        "builtin:plain" => Some(builtin_plain()),
        _ => None,
    }
}

fn text_style(foreground: &str) -> TextStyleSettings {
    TextStyleSettings {
        foreground: Some(foreground.to_owned()),
        ..TextStyleSettings::default()
    }
}

fn text_projection(foreground: &str) -> TextSettings {
    TextSettings {
        style: text_style(foreground),
        ..TextSettings::default()
    }
}

fn text_heading(foreground: &str) -> Option<HeadingLevelSettings> {
    Some(HeadingLevelSettings::Text {
        text: text_projection(foreground),
    })
}

fn rasterized_heading(
    foreground: &str,
    scale: f32,
    underline: bool,
    stroke_radius: u32,
) -> Option<HeadingLevelSettings> {
    Some(HeadingLevelSettings::Rasterized {
        text: text_projection(foreground),
        rasterized: RasterizedHeadingSettings {
            foreground: Some(foreground.to_owned()),
            scale: Some(scale),
            underline: Some(underline),
            stroke_radius: Some(stroke_radius),
            font_families: Some(Vec::new()),
        },
    })
}

fn table_text() -> TableTextSettings {
    TableTextSettings {
        cell: TextStyleSettings::default(),
        header: TextStyleSettings {
            bold: Some(true),
            ..TextStyleSettings::default()
        },
        border: TableTextBorderSettings {
            style: text_style("muted"),
            left: Some("│ ".to_owned()),
            column: Some(" │ ".to_owned()),
            right: Some(" │".to_owned()),
        },
    }
}

fn text_marker(marker_text: &str, foreground: &str) -> TextMarkerSettings {
    TextMarkerSettings {
        style: text_style(foreground),
        text: Some(marker_text.to_owned()),
    }
}

fn ordered_marker(suffix: &str, foreground: &str) -> OrderedMarkerSettings {
    OrderedMarkerSettings {
        style: text_style(foreground),
        suffix: Some(suffix.to_owned()),
    }
}

fn code_block_palette(colors: [&str; 9]) -> CodeBlockPaletteSettings {
    let [
        comment,
        variable,
        number,
        type_name,
        string,
        support,
        function,
        keyword,
        deprecated,
    ] = colors.map(str::to_owned);
    CodeBlockPaletteSettings {
        comment: Some(comment),
        variable: Some(variable),
        number: Some(number),
        type_name: Some(type_name),
        string: Some(string),
        support: Some(support),
        function: Some(function),
        keyword: Some(keyword),
        deprecated: Some(deprecated),
    }
}

// Built-in Theme settings. These use the same schema as user Theme files.

fn builtin_dark() -> ThemeSettings {
    ThemeSettings {
        palette: PaletteSettings {
            text: Some("#d8dee9".to_owned()),
            muted: Some("bright_black".to_owned()),
            accent: Some("cyan".to_owned()),
            secondary: Some("blue".to_owned()),
            inline_code: Some("yellow".to_owned()),
            surface: Some("black".to_owned()),
            border: Some("#8290a5".to_owned()),
            error: Some("bright_red".to_owned()),
            code_block: code_block_palette([
                "#65737e", "#bf616a", "#d08770", "#ebcb8b", "#a3be8c", "#96b5b4", "#8fa1b3",
                "#b48ead", "#ab7967",
            ]),
        },
        styles: ThemeStyleSettings {
            app: AppSettings {
                header: TextStyleSettings {
                    foreground: Some("accent".to_owned()),
                    bold: Some(true),
                    ..TextStyleSettings::default()
                },
                footer: text_style("muted"),
                diagnostic: text_style("error"),
            },
            document: DocumentSettings {
                body: TextStyleSettings::default(),
                paragraph: TextStyleSettings::default(),
                heading: HeadingSettings {
                    base: TextSettings {
                        style: TextStyleSettings {
                            bold: Some(true),
                            ..TextStyleSettings::default()
                        },
                        ..TextSettings::default()
                    },
                    h1: rasterized_heading("#46d2e1", 2.0, true, 2),
                    h2: rasterized_heading("#699bf5", 1.5, true, 1),
                    h3: text_heading("secondary"),
                    h4: text_heading("secondary"),
                    h5: text_heading("secondary"),
                    h6: text_heading("secondary"),
                },
                emphasis: TextStyleSettings {
                    italic: Some(true),
                    ..TextStyleSettings::default()
                },
                strong: TextStyleSettings {
                    bold: Some(true),
                    ..TextStyleSettings::default()
                },
                strikethrough: TextStyleSettings {
                    strikethrough: Some(true),
                    ..TextStyleSettings::default()
                },
                link: LinkSettings {
                    label: TextStyleSettings {
                        foreground: Some("secondary".to_owned()),
                        underline: Some(true),
                        ..TextStyleSettings::default()
                    },
                    destination: TextSettings {
                        style: text_style("muted"),
                        prefix: Some(" <".to_owned()),
                        suffix: Some(">".to_owned()),
                    },
                    standalone_destination: TextSettings {
                        style: text_style("muted"),
                        prefix: Some("<".to_owned()),
                        suffix: Some(">".to_owned()),
                    },
                },
                inline_code: text_projection("inline_code"),
                code_block: CodeBlockSettings {
                    text: TextStyleSettings::default(),
                    label: text_projection("muted"),
                },
                quote: QuoteSettings {
                    text: TextStyleSettings {
                        italic: Some(true),
                        ..TextStyleSettings::default()
                    },
                    marker: text_marker("│ ", "muted"),
                },
                list: ListSettings {
                    item: TextStyleSettings::default(),
                    unordered: text_marker("• ", "accent"),
                    ordered: ordered_marker(". ", "accent"),
                    task_checked: text_marker("[✓] ", "green"),
                    task_unchecked: text_marker("[ ] ", "muted"),
                    indent_cells: Some(2),
                },
                rule: RuleSettings {
                    style: text_style("muted"),
                    character: Some("─".to_owned()),
                },
                table: Some(TableSettings::Rasterized {
                    text: table_text(),
                    rasterized: RasterizedTableSettings {
                        cell: RasterizedTableTextSettings {
                            foreground: Some("text".to_owned()),
                            background: Some("none".to_owned()),
                            bold: Some(false),
                            italic: Some(false),
                            underline: Some(false),
                            strikethrough: Some(false),
                        },
                        header: RasterizedTableTextSettings {
                            bold: Some(true),
                            ..RasterizedTableTextSettings::default()
                        },
                        border: RasterizedTableBorderSettings {
                            color: Some("border".to_owned()),
                        },
                        font_points: Some(10.0),
                        horizontal_inset_points: Some(5.0),
                        vertical_inset_points: Some(3.0),
                        rule_gap_points: Some(5.0),
                    },
                }),
                math: MathSettings {
                    inline: Some(MathElementSettings::Rasterized {
                        text: text_projection("inline_code"),
                        rasterized: RasterizedMathSettings {
                            foreground: Some("text".to_owned()),
                            font_points: Some(11.0),
                            max_rows: Some(1),
                        },
                    }),
                    display: Some(MathElementSettings::Rasterized {
                        text: text_projection("inline_code"),
                        rasterized: RasterizedMathSettings {
                            foreground: Some("text".to_owned()),
                            font_points: Some(16.0),
                            max_rows: Some(4),
                        },
                    }),
                },
                image: Some(ImageSettings::Rasterized {
                    text: TextSettings {
                        style: text_style("muted"),
                        prefix: Some("[image: ".to_owned()),
                        suffix: Some("]".to_owned()),
                    },
                    rasterized: RasterizedImageSettings { max_rows: Some(20) },
                }),
                html: HtmlSettings {
                    inline: text_style("muted"),
                    block: text_style("muted"),
                },
            },
        },
    }
}

fn builtin_light() -> ThemeSettings {
    let mut theme = builtin_dark();
    theme.palette = PaletteSettings {
        text: Some("#24292f".to_owned()),
        muted: Some("#6e7781".to_owned()),
        accent: Some("#0969da".to_owned()),
        secondary: Some("#8250df".to_owned()),
        inline_code: Some("#953800".to_owned()),
        surface: Some("#ffffff".to_owned()),
        border: Some("#8c959f".to_owned()),
        error: Some("#cf222e".to_owned()),
        code_block: code_block_palette([
            "#65737e", "#bf616a", "#d08770", "#ebcb8b", "#a3be8c", "#96b5b4", "#8fa1b3", "#b48ead",
            "#ab7967",
        ]),
    };
    for heading in [
        &mut theme.styles.document.heading.h1,
        &mut theme.styles.document.heading.h2,
        &mut theme.styles.document.heading.h3,
        &mut theme.styles.document.heading.h4,
        &mut theme.styles.document.heading.h5,
        &mut theme.styles.document.heading.h6,
    ] {
        match heading
            .as_mut()
            .expect("built-in heading must be configured")
        {
            HeadingLevelSettings::Text { text } => {
                text.style.foreground = Some("accent".to_owned());
            }
            HeadingLevelSettings::Rasterized { text, rasterized } => {
                rasterized.foreground = Some("accent".to_owned());
                text.style.foreground = Some("accent".to_owned());
            }
        }
    }
    theme
}

fn builtin_plain() -> ThemeSettings {
    let plain = TextStyleSettings {
        foreground: Some("none".to_owned()),
        background: Some("none".to_owned()),
        bold: Some(false),
        italic: Some(false),
        underline: Some(false),
        strikethrough: Some(false),
        dim: Some(false),
        reverse: Some(false),
    };
    let mut theme = ThemeSettings::default();
    theme.styles.app.header = plain.clone();
    theme.styles.app.footer = plain.clone();
    theme.styles.app.diagnostic = plain.clone();
    theme.styles.document.body = plain.clone();
    theme.styles.document.paragraph = plain.clone();
    theme.styles.document.emphasis = plain.clone();
    theme.styles.document.strong = plain.clone();
    theme.styles.document.strikethrough = plain.clone();
    theme.styles.document.link.label = plain.clone();
    theme.styles.document.link.destination.style = plain.clone();
    theme.styles.document.link.standalone_destination.style = plain.clone();
    theme.styles.document.inline_code.style = plain.clone();
    theme.styles.document.code_block.text = plain.clone();
    theme.styles.document.code_block.label.style = plain.clone();
    theme.styles.document.quote.text = plain.clone();
    theme.styles.document.quote.marker.style = plain.clone();
    theme.styles.document.list.item = plain.clone();
    theme.styles.document.list.unordered.style = plain.clone();
    theme.styles.document.list.ordered.style = plain.clone();
    theme.styles.document.list.task_checked.style = plain.clone();
    theme.styles.document.list.task_unchecked.style = plain.clone();
    theme.styles.document.rule.style = plain.clone();
    theme.styles.document.html.inline = plain.clone();
    theme.styles.document.html.block = plain.clone();
    for heading in [
        &mut theme.styles.document.heading.h1,
        &mut theme.styles.document.heading.h2,
        &mut theme.styles.document.heading.h3,
        &mut theme.styles.document.heading.h4,
        &mut theme.styles.document.heading.h5,
        &mut theme.styles.document.heading.h6,
    ] {
        *heading = Some(HeadingLevelSettings::Text {
            text: TextSettings {
                style: plain.clone(),
                ..TextSettings::default()
            },
        });
    }
    let mut table = table_text();
    table.cell = plain.clone();
    table.header = plain.clone();
    table.border.style = plain.clone();
    theme.styles.document.table = Some(TableSettings::Text { text: table });
    theme.styles.document.math.inline = Some(MathElementSettings::Text {
        text: TextSettings {
            style: plain.clone(),
            ..TextSettings::default()
        },
    });
    theme.styles.document.math.display = Some(MathElementSettings::Text {
        text: TextSettings {
            style: plain.clone(),
            ..TextSettings::default()
        },
    });
    theme.styles.document.image = Some(ImageSettings::Text {
        text: TextSettings {
            style: plain,
            ..TextSettings::default()
        },
    });
    theme
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn temporary_directory() -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should follow Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("raden-theme-{suffix}"));
        fs::create_dir_all(&path).expect("theme fixture directory should be writable");
        path
    }

    #[test]
    fn documented_complete_theme_example_resolves() {
        let file = toml::from_str::<ThemeFile>(include_str!("../examples/themes/custom.toml"))
            .expect("complete Theme example should parse");
        let mut settings = builtin_dark();
        settings.merge(file.theme);

        Theme::from_settings("custom", &settings).expect("complete Theme example should resolve");
    }

    #[test]
    fn builtins_resolve_and_plain_choose_direct_element_variants() {
        let dark = resolve("builtin:dark", &ThemeSettings::default(), None, None)
            .expect("dark theme should resolve");
        let light = resolve("builtin:light", &ThemeSettings::default(), None, None)
            .expect("light theme should resolve");
        let plain = resolve("builtin:plain", &ThemeSettings::default(), None, None)
            .expect("plain theme should resolve");

        assert_ne!(dark.identity, light.identity);
        let HeadingStyle::Rasterized(rasterized) = dark.document.headings.level(1) else {
            panic!("built-in dark H1 should resolve directly to Rasterized");
        };
        assert_eq!(rasterized.scale, 2.0);
        assert_eq!(rasterized.fallback.style.bold, AttributeChoice::Enabled);
        assert!(matches!(
            plain.document.headings.level(1),
            HeadingStyle::Text(_)
        ));
        assert!(matches!(plain.document.table, TableStyle::Text(_)));
        assert!(matches!(plain.document.math.display, MathStyle::Text(_)));
        assert!(matches!(plain.document.image, ImageStyle::Text(_)));
    }

    #[test]
    fn user_theme_inherits_and_overrides_recursively() {
        let directory = temporary_directory();
        fs::write(
            directory.join("paper.toml"),
            "extends = \"builtin:light\"\n\n[palette]\naccent = \"#123456\"\n\n[styles.document.heading.h1]\nrenderer = \"text\"\n",
        )
        .expect("theme fixture should be writable");

        let theme = resolve("paper", &ThemeSettings::default(), Some(&directory), None)
            .expect("inherited theme should resolve");

        fs::remove_dir_all(directory).expect("theme fixture should be removable");
        assert!(matches!(
            theme.document.headings.level(1),
            HeadingStyle::Text(_)
        ));
        let ColorChoice::Set(header) = theme.app.header.foreground else {
            panic!("header should have a foreground color");
        };
        assert_eq!(header.rgb, [0x12, 0x34, 0x56]);
    }

    #[test]
    fn inheritance_cycles_are_actionable_errors() {
        let directory = temporary_directory();
        fs::write(directory.join("a.toml"), "extends = \"b\"\n")
            .expect("theme fixture should be writable");
        fs::write(directory.join("b.toml"), "extends = \"a\"\n")
            .expect("theme fixture should be writable");

        let error = resolve("a", &ThemeSettings::default(), Some(&directory), None)
            .expect_err("cycle should fail");

        fs::remove_dir_all(directory).expect("theme fixture should be removable");
        assert!(error.to_string().contains("cycle"));
    }

    #[test]
    fn invalid_palette_reference_is_rejected() {
        let overrides = toml::from_str::<ThemeSettings>(
            "[styles.app.header]\nforeground = \"missing-token\"\n",
        )
        .expect("partial theme should parse");
        let error = resolve("builtin:dark", &overrides, None, None)
            .expect_err("unknown palette reference should fail");

        assert!(error.to_string().contains("missing-token"));
    }

    #[test]
    fn heading_can_resolve_directly_to_text() {
        let overrides = toml::from_str::<ThemeSettings>(
            r##"
[styles.document.heading.h1]
renderer = "text"

[styles.document.heading.h1.text]
foreground = "#123456"
background = "surface"
bold = true
prefix = "▌ "
suffix = " ◀"
"##,
        )
        .expect("heading text Theme should parse");

        let theme = resolve("builtin:dark", &overrides, None, None)
            .expect("heading text Theme should resolve");
        let heading = theme.document.headings.level(1);

        let HeadingStyle::Text(text) = heading else {
            panic!("text must resolve to HeadingStyle::Text");
        };
        assert_eq!(text.prefix, "▌ ");
        assert_eq!(text.suffix, " ◀");
        let ColorChoice::Set(foreground) = text.style.foreground else {
            panic!("heading should have a foreground color");
        };
        assert_eq!(foreground.rgb, [0x12, 0x34, 0x56]);
        assert!(matches!(text.style.background, ColorChoice::Set(_)));
        assert_eq!(text.style.bold, AttributeChoice::Enabled);
        assert!(heading.rasterized().is_none());
    }

    #[test]
    fn every_semantic_text_role_accepts_the_complete_text_style() {
        let overrides = toml::from_str::<ThemeSettings>(
            r##"
[styles.document.emphasis]
foreground = "#123456"
background = "surface"
bold = true
italic = false
underline = true
strikethrough = true
dim = true
reverse = true
"##,
        )
        .expect("the common text style should parse for emphasis");

        let theme = resolve("builtin:dark", &overrides, None, None)
            .expect("the common text style should resolve for emphasis");
        let style = theme.document.emphasis;

        assert!(matches!(style.foreground, ColorChoice::Set(_)));
        assert!(matches!(style.background, ColorChoice::Set(_)));
        assert_eq!(style.bold, AttributeChoice::Enabled);
        assert_eq!(style.italic, AttributeChoice::Disabled);
        assert_eq!(style.underline, AttributeChoice::Enabled);
        assert_eq!(style.strikethrough, AttributeChoice::Enabled);
        assert_eq!(style.dim, AttributeChoice::Enabled);
        assert_eq!(style.reverse, AttributeChoice::Enabled);
    }

    #[test]
    fn text_heading_rejects_rasterized_settings() {
        let error = toml::from_str::<ThemeSettings>(
            r#"
[styles.document.heading.h1]
renderer = "text"

[styles.document.heading.h1.text]
foreground = "accent"

[styles.document.heading.h1.rasterized]
foreground = "accent"
scale = 2.0
"#,
        )
        .expect_err("a text heading must not accept rasterized settings");

        assert!(error.to_string().contains("rasterized"));
    }

    #[test]
    fn rule_character_must_occupy_exactly_one_terminal_cell() {
        let overrides =
            toml::from_str::<ThemeSettings>("[styles.document.rule]\ncharacter = \"wide\"\n")
                .expect("rule Theme should parse");

        let error = resolve("builtin:dark", &overrides, None, None)
            .expect_err("multi-cell rule character should fail");

        assert!(error.to_string().contains("exactly one terminal cell"));
    }

    #[test]
    fn code_block_palette_reports_the_invalid_named_role() {
        let overrides =
            toml::from_str::<ThemeSettings>("[palette.code_block]\nkeyword = \"not-a-color\"\n")
                .expect("partial Theme should parse");
        let error = resolve("builtin:dark", &overrides, None, None)
            .expect_err("invalid syntax color should fail");

        assert!(error.to_string().contains("palette.code_block.keyword"));
    }

    #[test]
    fn code_block_palette_rejects_container_colors() {
        let error =
            toml::from_str::<ThemeSettings>("[palette.code_block]\nbackground = \"#000000\"\n")
                .expect_err("code-block background belongs to the document style");

        assert!(error.to_string().contains("background"));
    }

    #[test]
    fn rasterized_table_header_overlays_cell_style() {
        let overrides = toml::from_str::<ThemeSettings>(
            r##"
[styles.document.table]
renderer = "rasterized"

[styles.document.table.rasterized.cell]
foreground = "#123456"
background = "#654321"

[styles.document.table.rasterized.header]
bold = false
italic = true
"##,
        )
        .expect("table raster styles should parse");
        let theme = resolve("builtin:dark", &overrides, None, None)
            .expect("table raster styles should resolve");
        let table = theme
            .document
            .table
            .rasterized()
            .expect("table should remain rasterized");

        assert_eq!(table.header.foreground, table.cell.foreground);
        assert_eq!(table.header.background, table.cell.background);
        assert!(!table.header.bold);
        assert!(table.header.italic);
    }

    #[test]
    fn arrays_replace_instead_of_appending_across_inheritance() {
        let directory = temporary_directory();
        fs::write(
            directory.join("base.toml"),
            "extends = \"builtin:dark\"\n\n[styles.document.heading.h1]\nrenderer = \"rasterized\"\n\n[styles.document.heading.h1.text]\nforeground = \"accent\"\n\n[styles.document.heading.h1.rasterized]\nforeground = \"accent\"\nscale = 2.0\nfont_families = [\"First\", \"Second\"]\n",
        )
        .expect("base Theme should be writable");
        fs::write(
            directory.join("child.toml"),
            "extends = \"base\"\n\n[styles.document.heading.h1]\nrenderer = \"rasterized\"\n\n[styles.document.heading.h1.rasterized]\nfont_families = [\"Replacement\"]\n",
        )
        .expect("child Theme should be writable");

        let theme = resolve("child", &ThemeSettings::default(), Some(&directory), None)
            .expect("child Theme should resolve");

        fs::remove_dir_all(directory).expect("Theme fixture should be removable");
        let heading = theme
            .document
            .headings
            .level(1)
            .rasterized()
            .expect("H1 should be rasterized");
        assert_eq!(heading.font_families, ["Replacement"]);
        assert_eq!(heading.scale, 2.0, "unspecified scalar should inherit");
    }

    #[test]
    fn auto_selection_uses_urushi_background_classification() {
        let light = resolve(
            "builtin:auto",
            &ThemeSettings::default(),
            None,
            Some(TerminalBackground::new(u16::MAX, u16::MAX, u16::MAX)),
        )
        .expect("light background should resolve");
        let dark = resolve(
            "builtin:auto",
            &ThemeSettings::default(),
            None,
            Some(TerminalBackground::new(0, 0, 0)),
        )
        .expect("dark background should resolve");

        assert_eq!(light.name, "builtin:light");
        assert_eq!(dark.name, "builtin:dark");
    }

    #[test]
    fn unknown_theme_fields_are_rejected() {
        let error = toml::from_str::<ThemeSettings>("[palette]\nunknown = \"#ffffff\"\n")
            .expect_err("unknown fields must be rejected");

        assert!(error.to_string().contains("unknown"));
    }
}
