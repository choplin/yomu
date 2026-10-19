# Read Markdown without leaving the terminal

Yomu keeps prose as native terminal text and renders the details that benefit
from extra fidelity. The result stays useful in every terminal.

## Built for focused reading

- Open a local file or pipe Markdown through standard input
- Keep your place while the terminal resizes and content reflows
- Read code, tables, math, and local images with text fallbacks

> Understanding-critical elements can look rich without turning the whole
> document into a picture.

## Syntax-aware code

```rust
fn main() {
    println!("hello from yomu");
}
```

## One document, graceful rendering

| Content | Rich presentation | Text fallback |
| --- | --- | --- |
| Headings | Rasterized type | Styled text |
| Tables | Typeset layout | Cell-aligned text |
| Math | Typeset formula | LaTeX source |

Inline styles remain readable: *emphasis*, **strong**, `code`, and links.
