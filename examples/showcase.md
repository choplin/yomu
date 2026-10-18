# yomu Markdown showcase

This document exercises the ordinary Markdown supported by the initial
one-file viewer. Resize the terminal while reading and confirm that the same
logical content remains near the top of the viewport.

## Inline styles

Plain text can contain *emphasis*, **strong emphasis**, ~~strikethrough~~,
`inline code`, and a [link to the yomu repository](https://example.com/yomu).
Inline math is retained as source text: $E = mc^2$.

## Lists and tasks

- Source-aware Markdown parsing
- Width-dependent terminal layout
  - Nested list content
  - A deliberately long nested item that wraps across terminal rows and makes
    source-anchored scrolling easy to inspect
- Safe terminal restoration

1. Open one local file
2. Read from top to bottom
3. Resize, scroll, and exit

- [x] Parse Markdown events with UTF-8 byte ranges
- [x] Preserve ranges in layout fragments
- [ ] Add comments in a later milestone

## Quotation

> The body stays terminal text, and only the parts that aid understanding
> should shine.

## Code

Inline code looks like `yomu examples/showcase.md`.

```rust
fn main() {
    println!("hello from yomu");
}
```

## Table

| Capability | Initial viewer | Later work |
| --- | --- | --- |
| Markdown text | Available | Refine styling |
| Source ranges | Available | Selection anchors |
| Images | Kitty or text fallback | Sixel |
| Math | Source fallback | Raster rendering |

## Math events

Inline: $a^2 + b^2 = c^2$

Display:

$$
\int_0^1 x^2\,dx = \frac{1}{3}
$$

Supported subset fixture: $\alpha + \beta \leq \sqrt{x_1^2 + x_2^2}$

$$
\sum_{i=1}^{n} i = \frac{n(n+1)}{2} \longrightarrow n \in \mathbb{N}
$$

Invalid conversion fallback: $\unknowncommand{x}$

## Horizontal rule

---

## Local image

This bundled PNG exercises successful Kitty rendering without any setup:

![A shell inlay detail](fixtures/shell-inlay.png)

## Error and fallback samples

Raw inline HTML remains visible but muted: <kbd>q</kbd> exits the viewer.

This separate missing image intentionally demonstrates a readable diagnostic
without interrupting reading:

![An intentionally missing image](fixtures/missing-image.png)

## Navigation checkpoint

If you reached this section using `j`, arrow keys, Page Down, Space, or
`d`, vertical navigation is working. Press `g` to return to the beginning,
`G` to come back here, then `q` or Escape to exit.
