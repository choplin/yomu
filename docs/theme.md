# Theme and element rendering

Raden Theme is the serializable application-semantic layer that maps Markdown
and reader chrome to Urushi styles plus Raden's typed raster parameters. It is
not a second terminal capability or graphics protocol system.

## Ownership boundary

Urushi owns terminal background observation and classification, low-level
colors and text styles, terminal capability lowering, graphics backend
selection, protocol lifecycle, and runtime fallback. Raden owns the `app` and
`document` semantic roles, Theme discovery and inheritance, user overrides,
and the element-specific decision to prepare text or raster content.

Only a validated `Theme` reaches `terminal`, `layout`, `ui`, and the
rasterizers. Those modules never merge partial configuration or interpret
unknown keys. Its rich elements are direct sum types: `HeadingStyle`, for
example, is either `Text(TextProjection)` or
`Rasterized(RasterizedHeading)`. `RasterizedHeading` lists its actual raster
settings and its required readable text projection directly. There is no runtime
state containing a presentation flag plus two potentially active render
styles. Text styles lower to Urushi values at View construction. Raster
settings remain Raden data and are included in raster cache identity.

## Resolution

The deterministic order is:

1. internal defaults;
2. the selected Theme's `extends` chain, oldest base first;
3. the selected Theme;
4. `config.toml` `theme_overrides`.

One Theme may extend one base. A repeated name in the active chain is an error.
Tables merge recursively, scalar fields replace, and arrays replace as a whole.
Rich-element payloads merge recursively while `renderer` stays the same;
changing `renderer` replaces the complete variant. Explicit `false`, `none`,
and an empty array therefore remain distinct from an omitted value.

The base selector is `config.toml`'s `theme`, unless `--theme` is present.
Config overrides apply after either selector. `builtin:auto` asks Urushi to
classify the observed background, with dark as the unavailable-observation
fallback; user Themes extend an explicit light or dark base so their inheritance
result stays deterministic.

## Settings boundary

The serializable `ThemeSettings` tree mirrors Raden's app and Markdown roles.
Every text-bearing role uses the same `TextStyleSettings`. A text projection
adds only the notation its role requires: prefix/suffix around source text,
`text` for a fixed marker, or the single-cell `character` repeated by a
thematic rule. Rich elements declare a `renderer`, which selects the
corresponding direct settings variant.

Table text follows Urushi's semantic presentation roles: `cell`, `header`, and
`border`. The border section owns both its text style and the `left`, `column`,
and `right` strings used by Raden's source-mapped text projection. It is kept as
`TableTextProjection` rather than presented as an Urushi `Table`: Raden must
retain a source range for each Markdown cell, while Urushi owns renderer-neutral
table composition without Markdown source identity. Rasterized table geometry
remains a separate Raden-specific section. Its `cell` and `header` text roles
control foreground, background, bold, italic, underline, and strikethrough;
`header` overlays `cell`. Its `border.color` is independent of both text roles.

Every rich element has a `text` section. A text variant contains only that
projection. A rasterized variant contains both `text` and `rasterized`
sections, with `text` serving as its readable fallback. Overrides with the same
renderer merge those sections; selecting the other renderer replaces the
variant.

The complete user-facing examples live outside `docs/`:

- [`examples/config.toml`](../examples/config.toml) shows the application
  configuration file.
- [`examples/themes/custom.toml`](../examples/themes/custom.toml) covers the
  complete Theme tree.

Semantic styles are overlays, not independent complete styles. They compose in
this fixed order; a later role replaces only the fields it explicitly sets:

1. `body`;
2. block context in document order: `paragraph`, `quote.text`, `list.item`,
   `code_block.text`, `table.text.cell`, `table.text.header`, math, image, HTML,
   and the resolved heading text (`heading.base` merged with its H1-H6 text);
3. a code-block syntax role, which replaces only the foreground;
4. inline semantics: `emphasis`, `strong`, `strikethrough`, `link.label`, and
   `inline_code`;
5. auxiliary text such as a link destination, code label, marker, rule, or
   table border.

Thus a table header inherits unspecified cell fields, and a strong link inside
a quote keeps all three meanings. Explicit `false` and `none` let a later role
remove an inherited attribute or color.

Palette values are fixed semantic keys rather than an untyped map. Palette
definitions contain literal ANSI colors or `#rrggbb`; style color fields refer
to those keys explicitly. `palette.inline_code` is therefore a reusable color
token, while `styles.document.inline_code` owns the actual inline-code style.

`styles.document.code_block.text` exclusively owns the block's default text
and background. `palette.code_block` contains only syntax-token foregrounds:
`comment`, `variable`, `number`, `type_name`, `string`, `support`, `function`,
`keyword`, and `deprecated`. Plain code inherits the document style instead of
having another palette owner. Parsed documents retain these stable semantic
roles, so selecting a Theme controls highlighting without reparsing Markdown.
A style may refer to a palette key, a supported ANSI color name, `#rrggbb`, or
`none` where clearing is allowed. Runtime color choices are explicitly
`Inherit`, `Set(color)`, or `Reset`; text attributes are `Inherit`, `Enabled`,
or `Disabled`, so contradictory combinations cannot be represented. Unknown
fields and references fail before the terminal runtime starts.

## Invariants

- Theme changes affect pixels and therefore participate in every raster cache
  identity, including ordinary image assets.
- Theme changes trigger ordinary width-dependent re-layout; the viewport stays
  anchored to source byte positions rather than old rows.
- Backend choice, terminal capability, viewport width, cache capacity, and
  resource safety limits are not Theme settings.
- No Theme or raster failure permits content to disappear.
