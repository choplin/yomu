#!/usr/bin/env bash

set -euo pipefail

# ttyd exposes the terminal grid but leaves the PTY pixel dimensions unset.
# Supply the Menlo 18 px cell geometry used by record.mjs so sixel layouts can
# align rasterized elements to the same grid rendered by the capture terminal.
sleep 0.5
python3 - <<'PY'
import fcntl
import struct
import termios

with open("/dev/tty", "rb+", buffering=0) as tty:
    rows, columns, _, _ = struct.unpack(
        "HHHH",
        fcntl.ioctl(tty.fileno(), termios.TIOCGWINSZ, bytes(8)),
    )
    fcntl.ioctl(
        tty.fileno(),
        termios.TIOCSWINSZ,
        struct.pack("HHHH", rows, columns, columns * 11, rows * 22),
    )
PY

exec env -u NO_COLOR \
    TERM=xterm-256color \
    COLORTERM=truecolor \
    ./target/release/yomu \
    --graphics sixel \
    --theme builtin:dark \
    assets/screenshots/demo.md
