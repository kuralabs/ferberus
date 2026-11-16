#!/usr/bin/env bash
# Run the unmodified upstream Cerberus test suite against ferberus.
# Usage (inside the dev container): scripts/upstream-tests.sh [3.14 ...] [-- pytest args]
set -euo pipefail
versions=()
while [ $# -gt 0 ] && [ "$1" != "--" ]; do versions+=("$1"); shift; done
[ "${1:-}" = "--" ] && shift
[ ${#versions[@]} -eq 0 ] && versions=(3.14)
UPSTREAM="${CERBERUS_CHECKOUT:-/opt/cerberus}"
git -C "$UPSTREAM" diff --quiet HEAD -- || { echo "upstream checkout is modified" >&2; exit 1; }
for v in "${versions[@]}"; do
    echo "==> upstream suite against ferberus on CPython $v"
    (cd /tmp && FERBERUS_UPSTREAM_PACKAGE="$UPSTREAM/cerberus" \
        "/opt/venvs/$v/bin/python" -m pytest -p ferberus.compat -p no:cacheprovider \
        --rootdir "$UPSTREAM" -c "$UPSTREAM/pyproject.toml" "$UPSTREAM/cerberus/tests" -q "$@")
done
