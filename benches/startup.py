#!/usr/bin/env python3
"""Compare Raden first-frame latency in a deterministic pseudo-terminal."""

from __future__ import annotations

import argparse
import csv
import errno
import fcntl
import hashlib
import json
import math
import os
import pty
import select
import shutil
import signal
import statistics
import struct
import subprocess
import sys
import tempfile
import termios
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Sequence


CAPABILITY_QUERY_END = b"\x1b[5n"
CAPABILITY_RESPONSE = (
    b"\x1b_Gi=31;EINVAL\x1b\\"
    b"\x1b[?1;2c"
    b"\x1bP1+r436f=323536\x1b\\"
    b"\x1bP1+r524742=38\x1b\\"
    b"\x1b[0n\n"
)
BACKGROUND_QUERY = b"\x1b]11;?\x1b\\"
BACKGROUND_RESPONSE = b"\x1b]11;rgb:0000/0000/0000\x1b\\\n"
MAX_CAPTURE_BYTES = 1024 * 1024


class BenchmarkError(Exception):
    """A benchmark setup or warm-up failed."""


@dataclass(frozen=True)
class Target:
    binary: Path
    sha256: str


@dataclass(frozen=True)
class Observation:
    capability_query_ms: float | None
    first_frame_ms: float


@dataclass(frozen=True)
class Sample:
    mode: str
    iteration: int
    observation: Observation | None = None
    error: str | None = None


def parse_args(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Measure Raden's first complete terminal frame in a fixed PTY"
    )
    parser.add_argument(
        "--samples", type=positive_int, default=20, help="recorded runs per mode"
    )
    parser.add_argument(
        "--warmups", type=nonnegative_int, default=2, help="unrecorded runs per mode"
    )
    parser.add_argument(
        "--mode",
        action="append",
        choices=("auto", "text"),
        help="graphics mode to measure; repeat for a matrix (default: auto and text)",
    )
    parser.add_argument(
        "--document",
        type=Path,
        default=Path("benches/fixtures/startup.md"),
        help="fixed benchmark document",
    )
    parser.add_argument(
        "--config",
        type=Path,
        default=Path("benches/fixtures/config.toml"),
        help="config copied into an isolated XDG_CONFIG_HOME",
    )
    parser.add_argument(
        "--marker",
        default="theme builtin:dark",
        help="footer text proving that the complete frame reached the PTY",
    )
    parser.add_argument("--columns", type=positive_int, default=100)
    parser.add_argument("--rows", type=positive_int, default=24)
    parser.add_argument("--timeout-seconds", type=positive_float, default=30.0)
    parser.add_argument(
        "--cooldown-ms",
        type=nonnegative_int,
        default=250,
        help="unmeasured delay between PTY processes",
    )
    parser.add_argument(
        "--output", type=Path, default=Path("target/startup-benchmark.csv")
    )
    args = parser.parse_args(argv)
    args.mode = args.mode or ["auto", "text"]
    if not args.marker:
        parser.error("--marker must not be empty")
    return args


def positive_int(value: str) -> int:
    parsed = int(value)
    if parsed <= 0:
        raise argparse.ArgumentTypeError("must be greater than zero")
    return parsed


def nonnegative_int(value: str) -> int:
    parsed = int(value)
    if parsed < 0:
        raise argparse.ArgumentTypeError("must not be negative")
    return parsed


def positive_float(value: str) -> float:
    parsed = float(value)
    if not math.isfinite(parsed) or parsed <= 0:
        raise argparse.ArgumentTypeError("must be a finite number greater than zero")
    return parsed


def checked_file(path: Path) -> Path:
    resolved = path.expanduser().resolve(strict=True)
    if not resolved.is_file():
        raise BenchmarkError(f"not a file: {resolved}")
    return resolved


def build_current_binary(repository: Path) -> Target:
    try:
        subprocess.run(
            ["cargo", "build", "--release", "--locked", "--bin", "raden"],
            cwd=repository,
            check=True,
        )
        metadata = subprocess.run(
            ["cargo", "metadata", "--format-version", "1", "--no-deps"],
            cwd=repository,
            check=True,
            stdout=subprocess.PIPE,
            text=True,
        )
    except FileNotFoundError as error:
        raise BenchmarkError("cargo is required to build the benchmark target") from error
    except subprocess.CalledProcessError as error:
        raise BenchmarkError(f"cargo failed with status {error.returncode}") from error
    target_directory = Path(json.loads(metadata.stdout)["target_directory"])
    binary = checked_file(target_directory / "release" / "raden")
    return Target(binary, file_sha256(binary))


def observe(
    binary: Path,
    document: Path,
    config_root: Path,
    mode: str,
    columns: int,
    rows: int,
    timeout_seconds: float,
    marker: bytes,
) -> Observation:
    environment = os.environ.copy()
    environment["TERM"] = "xterm-256color"
    environment["XDG_CONFIG_HOME"] = os.fspath(config_root)
    started = time.perf_counter()
    pid, master = pty.fork()
    if pid == 0:
        os.execve(
            binary,
            [os.fspath(binary), "--graphics", mode, os.fspath(document)],
            environment,
        )

    deadline = started + timeout_seconds
    output = bytearray()
    capability_query_ms = None
    capability_answered = False
    background_answered = False
    resize_confirmed = False
    completed = False
    child_reaped = False
    try:
        fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
        while True:
            now = time.perf_counter()
            if now >= deadline:
                raise BenchmarkError(
                    f"timed out after {(now - started) * 1000:.3f} ms before "
                    f"first-frame marker; terminal output: {output_excerpt(output)}"
                )
            readable, _, _ = select.select([master], [], [], min(0.05, deadline - now))
            if readable:
                try:
                    chunk = os.read(master, 64 * 1024)
                except OSError as error:
                    if error.errno == errno.EIO:
                        chunk = b""
                    else:
                        raise
                if not chunk:
                    waited, status = os.waitpid(pid, os.WNOHANG)
                    if waited == pid:
                        child_reaped = True
                        description = describe_wait_status(status)
                    else:
                        description = "closed its PTY"
                    raise BenchmarkError(
                        f"process {description} before first-frame marker; "
                        f"terminal output: {output_excerpt(output)}"
                    )
                output.extend(chunk)
                if not resize_confirmed:
                    fcntl.ioctl(
                        master,
                        termios.TIOCSWINSZ,
                        struct.pack("HHHH", rows, columns, 0, 0),
                    )
                    os.kill(pid, signal.SIGWINCH)
                    resize_confirmed = True
                if len(output) > MAX_CAPTURE_BYTES:
                    raise BenchmarkError(
                        f"terminal output exceeded {MAX_CAPTURE_BYTES} bytes before first frame"
                    )
                if not capability_answered and CAPABILITY_QUERY_END in output:
                    capability_query_ms = (time.perf_counter() - started) * 1000
                    os.write(master, CAPABILITY_RESPONSE)
                    capability_answered = True
                if not background_answered and BACKGROUND_QUERY in output:
                    os.write(master, BACKGROUND_RESPONSE)
                    background_answered = True
                if marker in output:
                    observation = Observation(
                        capability_query_ms=capability_query_ms,
                        first_frame_ms=(time.perf_counter() - started) * 1000,
                    )
                    completed = True
                    return observation

            waited, status = os.waitpid(pid, os.WNOHANG)
            if waited == pid:
                child_reaped = True
                raise BenchmarkError(
                    f"process {describe_wait_status(status)} before first-frame marker; "
                    f"terminal output: {output_excerpt(output)}"
                )
    finally:
        if completed:
            try:
                os.write(master, b"q")
            except OSError:
                pass
            reap_deadline = time.perf_counter() + 0.25
            while time.perf_counter() < reap_deadline:
                try:
                    waited, _ = os.waitpid(pid, os.WNOHANG)
                except ChildProcessError:
                    child_reaped = True
                    break
                if waited == pid:
                    child_reaped = True
                    break
                time.sleep(0.01)
        if not child_reaped:
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        os.close(master)
        try:
            os.waitpid(pid, os.WNOHANG)
        except ChildProcessError:
            pass


def describe_wait_status(status: int) -> str:
    code = os.waitstatus_to_exitcode(status)
    return f"exited with status {code}" if code >= 0 else f"was killed by signal {-code}"


def output_excerpt(output: bytes) -> str:
    return output[-512:].decode("utf-8", errors="backslashreplace").encode("unicode_escape").decode()


def run_sample(
    target: Target,
    mode: str,
    iteration: int,
    args: argparse.Namespace,
    document: Path,
    config_root: Path,
) -> Sample:
    try:
        observation = observe(
            target.binary,
            document,
            config_root,
            mode,
            args.columns,
            args.rows,
            args.timeout_seconds,
            args.marker.encode(),
        )
        return Sample(mode, iteration, observation=observation)
    except (BenchmarkError, OSError) as error:
        return Sample(mode, iteration, error=str(error))


def file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def write_csv(
    path: Path,
    samples: Sequence[Sample],
    args: argparse.Namespace,
    target: Target,
    document: Path,
    config: Path,
) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", newline="", encoding="utf-8") as stream:
        writer = csv.writer(stream)
        writer.writerow(
            (
                "binary",
                "binary_sha256",
                "mode",
                "iteration",
                "columns",
                "rows",
                "cooldown_ms",
                "timeout_seconds",
                "document",
                "config",
                "marker",
                "success",
                "capability_query_ms",
                "first_frame_ms",
                "error",
            )
        )
        for sample in samples:
            observation = sample.observation
            writer.writerow(
                (
                    target.binary,
                    target.sha256,
                    sample.mode,
                    sample.iteration,
                    args.columns,
                    args.rows,
                    args.cooldown_ms,
                    args.timeout_seconds,
                    document,
                    config,
                    args.marker,
                    observation is not None,
                    ""
                    if observation is None or observation.capability_query_ms is None
                    else f"{observation.capability_query_ms:.3f}",
                    "" if observation is None else f"{observation.first_frame_ms:.3f}",
                    sample.error or "",
                )
            )


def percentile(values: Sequence[float], quantile: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    rank = max(1, math.ceil(quantile * len(ordered)))
    return ordered[rank - 1]


def display_stat(value: float | None) -> str:
    return "-" if value is None else f"{value:.3f}"


def print_summary(args: argparse.Namespace, target: Target, samples: Sequence[Sample]) -> None:
    print("Raden startup benchmark")
    print(f"  document: {args.document}")
    print(f"  config: {args.config}")
    print(f"  PTY: {args.columns}x{args.rows}")
    print(f"  cooldown: {args.cooldown_ms} ms")
    print(f"  warmups: {args.warmups}; samples: {args.samples}")
    print(f"  raw samples: {args.output}")
    print(f"  binary: {target.binary}")
    print(f"  sha256: {target.sha256}")
    print()
    print(
        f"{'mode':<10} {'ok':>7} {'failed':>7} "
        f"{'median ms':>12} {'p95 ms':>12} {'query med.':>12}"
    )
    for mode in args.mode:
        group = [sample for sample in samples if sample.mode == mode]
        observations = [sample.observation for sample in group if sample.observation]
        frames = [value.first_frame_ms for value in observations]
        queries = [
            value.capability_query_ms
            for value in observations
            if value.capability_query_ms is not None
        ]
        print(
            f"{mode:<10} {len(frames):>7} {len(group) - len(frames):>7} "
            f"{display_stat(statistics.median(frames) if frames else None):>12} "
            f"{display_stat(percentile(frames, 0.95)):>12} "
            f"{display_stat(statistics.median(queries) if queries else None):>12}"
        )
    print()
    print("Statistics include every successful recorded run; no outliers are removed.")


def run(args: argparse.Namespace) -> int:
    repository = Path(__file__).resolve().parent.parent
    target = build_current_binary(repository)
    document = checked_file(args.document)
    config = checked_file(args.config)

    with tempfile.TemporaryDirectory(prefix="raden-startup-benchmark-") as root:
        config_root = Path(root)
        config_directory = config_root / "raden"
        config_directory.mkdir()
        shutil.copyfile(config, config_directory / "config.toml")

        for mode in args.mode:
            for warmup in range(args.warmups):
                sample = run_sample(
                    target, mode, warmup + 1, args, document, config_root
                )
                if sample.error:
                    raise BenchmarkError(
                        f"{mode} warm-up {warmup + 1} failed: {sample.error}"
                    )
                time.sleep(args.cooldown_ms / 1000)

        samples = []
        for mode in args.mode:
            for iteration in range(args.samples):
                samples.append(
                    run_sample(
                        target,
                        mode,
                        iteration + 1,
                        args,
                        document,
                        config_root,
                    )
                )
                time.sleep(args.cooldown_ms / 1000)

    write_csv(args.output, samples, args, target, document, config)
    print_summary(args, target, samples)
    failures = sum(sample.error is not None for sample in samples)
    if failures:
        raise BenchmarkError(f"{failures} recorded run(s) failed; see {args.output}")
    return 0


def main() -> int:
    try:
        return run(parse_args(sys.argv[1:]))
    except (BenchmarkError, OSError) as error:
        print(f"startup benchmark failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
