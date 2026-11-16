#!/usr/bin/env bash
# Build ferberus and install it (editable) into the given interpreters' venvs.
# Usage (inside the dev container): scripts/develop.sh [3.14 3.13 ...]
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
versions=("$@")
[ ${#versions[@]} -eq 0 ] && versions=(3.14)
for v in "${versions[@]}"; do
    echo "==> maturin develop for CPython $v"
    VIRTUAL_ENV="/opt/venvs/$v" maturin develop --release --quiet
done
