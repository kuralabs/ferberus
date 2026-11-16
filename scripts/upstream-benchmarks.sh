#!/usr/bin/env bash
# Run Cerberus' bundled benchmarks against ferberus (unmodified, through the alias).
set -euo pipefail
UPSTREAM="${CERBERUS_CHECKOUT:-/opt/cerberus}"
cd /tmp && FERBERUS_UPSTREAM_PACKAGE="$UPSTREAM/cerberus" python -m pytest -p ferberus.compat -p no:cacheprovider \
    --rootdir "$UPSTREAM" -c "$UPSTREAM/pyproject.toml" "$UPSTREAM/cerberus/benchmarks" -q \
    --benchmark-columns=min,mean,stddev,rounds "$@"
