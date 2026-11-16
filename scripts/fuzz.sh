#!/usr/bin/env bash
# cargo-fuzz for the pure-Rust parts. Usage: scripts/fuzz.sh [seconds] [target]
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../fuzz"
seconds="${1:-60}"
target="${2:-regex_translate}"
# cargo-fuzz is a musl binary and would default to its own target triple
exec cargo +nightly-2026-11-10 fuzz run --target "$(rustc -vV | sed -n 's/^host: //p')" "$target" -- \
    -max_total_time="$seconds" -max_len=96
