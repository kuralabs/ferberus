#!/usr/bin/env bash
# Build, then run the upstream suite and the differential tests.
# Usage (inside the dev container): scripts/check.sh [pytest args for the differential tests]
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
scripts/develop.sh 3.14 2>&1 | grep -E "^(error|warning: unused)|Installed" -A12 || true
scripts/upstream-tests.sh 3.14 2>&1 | tail -3
python -m pytest tests -q -p no:cacheprovider -x "$@" 2>&1 | grep -E "^E   |passed|failed|error" | cut -c1-1500 | head -40
