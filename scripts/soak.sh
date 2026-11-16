#!/usr/bin/env bash
# Long differential fuzzing session on a private copy of the built package,
# so that rebuilding meanwhile does not disturb it.
# Usage (inside the dev container): scripts/soak.sh [examples per property] [pytest args]
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
examples="${1:-20000}"
shift || true
copy="$(mktemp -d /tmp/ferberus-soak.XXXXXX)"
cp -r python/ferberus tests "$copy/"
cd "$copy"
PYTHONPATH="$copy" HYPOTHESIS_PROFILE=custom HYPOTHESIS_MAX_EXAMPLES="$examples" \
    python -m pytest tests/differential tests/test_regex_fast.py -q -p no:cacheprovider -c /dev/null --rootdir "$copy" "$@"
