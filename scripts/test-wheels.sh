#!/usr/bin/env bash
# Run the unmodified upstream suite against built wheels, on every
# interpreter in $INTERPRETERS (used by .github/workflows/wheels.yml).
#
#   scripts/test-wheels.sh <dist dir> <upstream checkout>
#
# Needs uv. Every interpreter must have exactly one wheel in <dist dir>.
set -euo pipefail

dist="$(cd "$1" && pwd)"
upstream="$(cd "$2" && pwd)"
interpreters="${INTERPRETERS:?}"
work="$(mktemp -d)"

count=$(ls "$dist"/*.whl | wc -l)
expected=$(echo $interpreters | wc -w)
[ "$count" -eq "$expected" ] || { echo "expected $expected wheels, found $count"; ls "$dist"; exit 1; }

for v in $interpreters; do
    echo "::group::CPython $v"
    uv venv --python "$v" "$work/venv-$v"
    python="$work/venv-$v/bin/python"
    [ -x "$python" ] || python="$work/venv-$v/Scripts/python.exe"
    uv pip install --python "$python" --no-index --find-links "$dist" ferberus
    uv pip install --python "$python" pytest
    (cd "$work" && FERBERUS_UPSTREAM_PACKAGE="$upstream/cerberus" \
        "$python" -m pytest -p ferberus.compat -p no:cacheprovider \
        --rootdir "$upstream" -c "$upstream/pyproject.toml" "$upstream/cerberus/tests" -q)
    echo "::endgroup::"
done
