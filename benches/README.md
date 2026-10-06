# Startup benchmark

`startup.py` builds the current checkout in release mode, then measures the
latency from spawning that Raden executable to the first complete terminal
frame and the first raster graphic. It runs the real executable in a fixed-size
pseudo-terminal and replies to terminal capability queries with deterministic
responses.

The primary metric is `first_frame_ms`. The frame is complete when the footer
marker from the fixed theme reaches the PTY. `capability_query_ms` is a
diagnostic showing when terminal detection starts; it is not interchangeable
with the primary metric.

In `auto` mode, `first_graphics_ms` records the first Kitty image upload. The
benchmark positively reports Kitty support so this measures when a prepared
raster asset reaches the terminal, independent of the terminal running the
benchmark. Text mode does not produce this metric.

## What is fixed

- the document in `fixtures/startup.md`;
- the theme and rasterized heading font stack in `fixtures/config.toml`;
- a 100 by 24 cell PTY by default;
- terminal capability and background-color replies;
- an isolated `XDG_CONFIG_HOME`;
- a 250 ms unmeasured cooldown between PTY processes.

The fixture deliberately has both H1 and H2 headings configured with the same
font stack. This exercises reuse of resolved system fonts inside one process.
The final embedded font fallback remains available when none of those families
are installed. Font availability is machine-specific, so compare revisions on
the same host and in the same session.

## Running the benchmark

Run the benchmark from the repository root. It runs `cargo build --release
--locked --bin raden` before collecting samples, so the measured executable
always reflects the current checkout:

```console
nix develop -c python3 benches/startup.py \
  --samples 20 \
  --warmups 2 \
  --output target/startup-benchmark.csv
```

The default matrix records both `auto` and `text` graphics modes. `auto`
exercises rasterization; `text` is the control for startup work that is not
specific to raster graphics. Restrict a run with `--mode auto` or
`--mode text`. Use `--help` for fixture, PTY, timeout, cooldown, and marker
overrides.

Run on an otherwise idle machine. Do not compare a debug build with a release
build, and do not compare numbers collected with different fixtures or PTY
dimensions or cooldowns.

To compare another already-built revision without changing the current
checkout, pass its executable with `--binary`. The CSV records the resolved
path and SHA-256 hash; the caller is responsible for building that executable
in release mode from the intended revision.

## Reporting

The console summary reports the median and nearest-rank p95 of every successful
recorded run. Warm-ups are excluded. No successful sample is discarded as an
outlier, and failures are counted separately. The CSV is the source of truth.
It records the binary path and SHA-256 hash, all fixed settings, iteration,
both timings, and any failure message.

When reporting a result, include:

- the current revision and working-tree state;
- the recorded binary SHA-256 hash;
- host and operating system;
- fixture and PTY dimensions;
- warm-up and sample counts;
- median and p95 `first_frame_ms` and `first_graphics_ms`; and
- the raw CSV.
