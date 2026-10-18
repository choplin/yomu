# Yomu

Yomu is a terminal Markdown reader. It keeps prose as native terminal text and
uses Kitty Graphics or experimental sixel rendering for images, large
headings, tables, and math when the terminal supports them. Every rich element
retains a readable text fallback.

## Overview

- A Markdown reader for the terminal. The text body is rendered as normal
  terminal text; understanding-critical elements are rendered pixel-perfect
  via terminal graphics protocols.
- Images, headings, tables, and a documented subset of LaTeX math can render as
  raster graphics, with deterministic text degradation.
- Files and standard input are both supported; viewport navigation remains
  anchored to source content across resize and reflow.

## Installation

After the first release is published, install the crate with Cargo:

```console
cargo install yomu --locked
```

The same release's prebuilt archives can be installed with Homebrew or
cargo-binstall:

```console
brew install choplin/tap/yomu
cargo binstall yomu
```

Before the first release, or to install the latest source revision instead:

```console
cargo install --git https://github.com/choplin/yomu --locked
```

From a local checkout:

```console
cargo install --path . --locked
```

## Usage

Open a Markdown file:

```console
yomu examples/showcase.md
```

Markdown can also be piped through standard input. Use either an omitted path
or the conventional `-`; relative image paths are resolved from the current
working directory:

```console
cat examples/showcase.md | yomu
printf '# Hello\n' | yomu -
```

The showcase covers headings, inline styles, nested and ordered lists, task
lists, quotations, code, tables, math events, a horizontal rule, HTML, and an
image diagnostic. You can also replace it with any local Markdown path.
Use `examples/showcase-ja.md` for a focused Japanese typography, wrapping, and
fallback check.

Local PNG, JPEG, WebP, and GIF images render automatically through Kitty
Graphics or sixel when the terminal reports support. GIFs use their first
frame. Image paths are resolved
relative to the Markdown file's directory; remote URLs, absolute paths, paths
that escape that directory, unsupported formats, oversized images, and decode
failures stay readable as an inline diagnostic. Without usable graphics, the same elements use their text fallback.

Level-one and level-two headings also render as larger rasterized typography in
Kitty or through Sixel. They use an embedded Noto Sans
JP font so Japanese and Latin text render
consistently; level-three through level-six headings remain terminal text.
Unsupported terminals and rasterization failures retain readable heading text.
The bundled font is licensed under the SIL Open Font License 1.1; see
`assets/fonts/OFL-NotoSansJP.txt`.

Without configuration, Yomu asks Urushi to classify the terminal background
once before startup and selects its light or dark built-in Theme. Missing,
timed-out, or malformed background responses quietly use the dark Theme.

Create `$XDG_CONFIG_HOME/yomu/config.toml` (or
`~/.config/yomu/config.toml` when `XDG_CONFIG_HOME` is unset) to select an
explicit Theme and configure the reader:

```toml
theme = "builtin:dark"

[view]
max_width = 100
```

### Themes and element rendering

Yomu ships `builtin:dark`, `builtin:light`, and `builtin:plain`. Select one in
`config.toml` or override the selection for one invocation:

```console
yomu --theme builtin:light examples/showcase.md
```

`builtin:plain` uses terminal text for every rich element. `builtin:auto` is
the default; it asks Urushi to classify the observed terminal background and
falls back to the dark Theme when the terminal cannot provide usable evidence.
Theme selection does not change graphics backend selection. User Themes,
per-element rendering choices, and overrides follow the complete contract in
[`docs/theme.md`](docs/theme.md).

The main config may override only the fields it needs. CLI `--theme` replaces
the selected base Theme while these overrides still apply:

```toml
theme = "builtin:dark"

[theme_overrides.palette]
accent = "#ff87d7"

[theme_overrides.styles.app.header]
foreground = "accent"

[theme_overrides.styles.document.heading.h1]
renderer = "text"

[theme_overrides.styles.document.heading.h1.text]
foreground = "accent"
bold = true
```

See [`examples/config.toml`](examples/config.toml) for a complete application
configuration.

User Themes are TOML files in
`${XDG_CONFIG_HOME:-$HOME/.config}/yomu/themes/`. The filename without
`.toml` is the selection name; no duplicate name field is required:

[`examples/themes/custom.toml`](examples/themes/custom.toml) is a complete,
copyable Theme file covering the palette and every app/document role.

Resolution applies internal defaults, the single `extends` chain, the selected
Theme, then `theme_overrides`. Tables merge recursively, scalar values replace,
and arrays replace as a whole. Rich elements merge their fields while the
`renderer` stays the same; changing `renderer` replaces the complete variant.
An empty `font_families` array clears an inherited font list. `none` clears a
supported foreground or background.
Inheritance cycles, unknown keys, invalid colors or palette references, and
out-of-range raster values stop startup with a configuration error.

`styles.app` contains `header`, `footer`, and `diagnostic`. Document styling is
split by Markdown meaning rather than by the current renderer: `body`,
`paragraph`, `heading.base` plus `heading.h1` through `heading.h6`,
`emphasis`, `strong`, `strikethrough`, link label/destination, inline code,
code-block body/language label, quote body/marker, list and task markers, rule,
table cell/header/border, inline/display math, image, and inline/block HTML.
Text style supports foreground/background, bold, italic, underline,
strikethrough, dim, and reverse. Text projections that need terminal notation
also expose prefix/suffix or fixed marker text. Fixed marker sections use
`text`; ordered-list numbering keeps the source number and configures its
`suffix`. Thematic rules use the single-cell `character`, and table text follows
the `cell` / `header` / `border` roles used by Urushi tables. Yomu's
source-mapped table projection keeps its `left`, `column`, and `right` boundary
text inside the `border` section. Rasterized tables use the same three roles;
their header text style overlays the cell style, while `border.color` is
independent. Rasterized heading font overrides use `rasterized.font_families`.

Styles are semantic overlays: body first, then block context, code syntax,
inline semantics, and auxiliary marker/destination/border roles. Later roles
replace only fields they set. Table headers therefore inherit unspecified cell
fields. `styles.document.code_block.text` owns the block's default foreground
and background; `palette.code_block` contains only named syntax-token colors.
Palette keys such as `inline_code` are referenced explicitly from style color
fields rather than being applied implicitly.

Raster-capable elements accept `renderer = "text" | "rasterized"`. Every such
element has a `text` section. The text renderer uses it directly; the
rasterized renderer adds a sibling `rasterized` section and retains `text` as
its readable fallback. Omitting an element inherits the selected Theme's
complete variant.
See [`docs/theme.md`](docs/theme.md) for the complete ownership
and resolution contract.

The document body defaults to at most 100 terminal cells and is centered when
the terminal is wider. Set `view.max_width` to another positive cell count to
choose a different cap, or to `0` to use the full terminal width. Narrower
terminals always use all available columns. The header and footer remain at
terminal width. Invalid values, including negative numbers and non-integers,
cause config loading to fail with a parse error.

Heading `rasterized.font_families` are tried in order, followed by the embedded
Noto Sans JP fallback. Unavailable families are skipped unless none of the
configured families can be resolved. Use a Nerd Font `Mono` family when its
glyphs should retain terminal-like spacing.

GFM tables render through an embedded Typst-to-SVG-to-RGBA pipeline in the
selected Kitty or Sixel presentation. Their canvas is transparent so
the terminal background remains visible. Column widths fit
parsed cell content up to a deterministic cap and
only expand as far as needed, bounded by the viewport; wide cells wrap instead
of shrinking the text. Yomu determines the terminal height from the original
table rows and their precomputed cell wrapping, with one extra row of breathing
room. That logical height is converted through the terminal's measured cell
pixels, and the SVG is scaled uniformly to it; width follows from the SVG aspect
ratio. If the
viewport cannot give every column a readable minimum width, or if
table compilation or rasterization fails, yomu re-lays out every cell as
source-mapped terminal text. The renderer embeds its font and exposes no Typst
package, network, or filesystem resolver.

Tables use a booktabs-like paper style: no vertical grid or left/right border,
with strong top and bottom rules plus a lighter rule separating the bold header
from the body.

Inline and display math render through a TeX-to-Typst-to-SVG-to-RGBA pipeline.
Inline formulas remain in their surrounding logical line and fit one terminal
row; display formulas use a larger centered placement capped at four rows, with
roughly half a row of breathing room above and below the painted formula.
Raster canvases align exactly to terminal-cell pixels so the graphics protocol
does not rescale otherwise crisp inline glyphs during placement. The rasterizer
fits the actual painted glyph bounds, including accents and bearings, and keeps
transparent pixels above and below them to prevent vertical clipping.
The renderer embeds Typst's bundled New Computer Modern fonts and exposes no
Typst package, network, or filesystem resolver. Raster cache identity includes
the formula, inline/display form, target geometry, foreground render setting,
bundled fonts, converter version, rasterizer version, and resolved Theme
identity, so resize or Theme changes cannot reuse stale pixels.

The evidenced MVP subset includes ordinary symbols and relations, Greek
letters, superscripts and subscripts, fractions, roots, sums, integrals,
parenthesized expressions, and common arrows as demonstrated in
`examples/showcase.md`. This is intentionally not a claim of full LaTeX or
MathJax compatibility. Unsupported or malformed input remains visible with its
original `$...$` or `$$...$$` delimiters and a concise conversion or rendering
diagnostic.

Yomu enters the alternate screen, queries terminal capabilities and cell-pixel
size, and only then starts reading input events. A terminal response is primary
evidence; environment variables are used only when the protocol was not tested
or no usable response arrived. Automatic selection is deterministic:
Kitty, then sixel, then Text. Sixel remains experimental.

Override the automatic choice from the command line:

```console
yomu --graphics text examples/showcase.md
```

The accepted values are `auto`, `kitty`, `sixel`, and `text`. An explicit Kitty
or sixel choice fails with an actionable diagnostic when that protocol is not
available; it never silently chooses another backend. The same default can be
stored in the config file:

```toml
[graphics]
renderer = "auto"
```

The command-line option takes precedence over the config file. The footer shows
the active backend, labels sixel `experimental`, and includes a concise
degradation reason after a capability or rendering failure.

| Capability evidence | `auto` result | Notes |
| --- | --- | --- |
| Kitty and sixel | Kitty | Highest-priority supported backend |
| sixel only | sixel | Reported as experimental |
| no graphics response, no fallback hint | Text | Full text projection remains reviewable |
| authoritative graphics response contradicts an environment hint | Response result | Avoids stale `TERM`/session false positives |
| mid-session graphics failure | Text | Clear/reset, full text re-layout, then full redraw |

To keep hostile or accidental input bounded, a rasterized table may contain at
most 32 columns and 2,048 cells, occupy at most 256 terminal rows, and consume
at most 128 MiB. A layout rasterizes the four tables nearest the current source
anchor and allows at most 64 MiB of table pixels at once; other tables retain
their complete text projection until they approach the viewport. Exceeding a
per-table or layout limit also produces that readable projection with a concise
diagnostic instead of clipping or omitting source content.

The initial Reading mode uses these deliberately reversible bindings:

- `j` / `k` or arrow keys: scroll one rendered row
- `d` / `u`, Page Down / Page Up, or Space: scroll one page
- `g` / `G` or Home / End: jump to the start or end
- `q` or Escape: exit

The viewport is anchored to source byte positions rather than terminal cells,
so resizing reflows the document while keeping the same logical content in
view.

## Startup performance

The repository includes a repeatable PTY benchmark for measuring the first
complete frame of the current release build. See
[`benches/README.md`](benches/README.md) for its measurement contract and
usage.

## Text rendering check

Text mode requires no terminal graphics support:

```console
yomu --graphics text examples/showcase.md
```

Verify ordinary and Unicode prose, code, links and destinations, thematic
rules, headings, narrow and wide tables, valid and invalid math, the local
image label, and the long document scroll path. Resize the terminal and confirm
that the same source content remains near the viewport anchor, then quit with
`q` and confirm the normal shell screen is restored.

## Kitty graphics check

Use a Markdown fixture with level-one or level-two headings, or one that
references a local PNG, JPEG, WebP, or GIF, then run the viewer in a
Kitty-compatible terminal:

```console
yomu /path/to/fixture.md
```

Check the backend label in the footer and verify this lifecycle:

1. The image appears at its Markdown position with its original aspect ratio.
2. Scroll it completely off screen and back; no stale placement remains.
3. Resize narrower and wider; the image is replaced at the new size and the
   same source content remains near the viewport anchor.
4. Add missing, remote, unsupported, and corrupt image references; each becomes
   readable diagnostic text while valid images continue rendering.
5. Quit with `q`, then confirm the shell has no image placement left behind.
6. Verify H1 and H2 are visually distinct, Japanese text is intact, and narrow
   terminal widths wrap headings without clipping them.
7. Inspect the tables in both showcase files: headers, alignment, borders,
   Unicode, and wrapped cells remain legible; narrowing below the readable
   column minimum switches to text without dropping a cell.
8. Inspect the inline and display formulas: inline math stays with its prose,
   display math is centered and larger, and resizing produces crisp replacement
   pixels without stale placements.
9. Confirm the intentionally invalid math sample remains readable as source
   with a diagnostic while the supported formulas continue rendering.

The automated tests separately cover desired-frame reconciliation, upload and
placement replacement, stale deletion, cache ownership, path escape rejection,
decode fallback, table layout and overflow, Unicode, cache keys, failure
projection, math parse boundaries, supported conversion, invalid-input
fallback, inline/display layout, resize-sensitive cache keys, and source ranges.
Real-terminal checks remain necessary because
terminal graphics behavior cannot be observed through a test backend.

## Experimental sixel graphics check

Run the same representative Markdown fixture in a sixel terminal, such as xterm
with sixel enabled, foot, or mlterm. Confirm that `--graphics auto` selects
`sixel experimental`; use `--graphics sixel` to verify the explicit path. Run
that explicit command once in a non-sixel terminal and confirm it exits with an
unavailable-backend diagnostic that suggests `auto` or `text`.

Verify the initial render and the same scroll, resize, invalid-image, and quit
lifecycle listed for Kitty. In addition, confirm that every changed frame
clears the previous graphics occupancy and repaints the complete visible scene,
with no stale fragments after scrolling away and back. A brief full-screen
redraw is expected: portable partial sixel deletion is not reliable across the
target terminals.

## Development

The Nix development shell supplies the supported Rust toolchain and local
utilities:

```console
nix develop
cargo test --all-targets --all-features
```

Release automation and its non-publishing validation procedure are documented
in [`docs/release-automation.md`](docs/release-automation.md).

## License

Yomu is licensed under the [MIT License](LICENSE). The bundled Noto Sans JP
font remains licensed under the SIL Open Font License 1.1; see
[`assets/fonts/OFL-NotoSansJP.txt`](assets/fonts/OFL-NotoSansJP.txt).
