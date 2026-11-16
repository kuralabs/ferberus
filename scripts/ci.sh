#!/usr/bin/env bash
# Everything CI checks, run inside the dev container (locally: ./dev scripts/ci.sh).
#   scripts/ci.sh [lint|rust|python|bench]...     default: all stages
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

PYTHONS="${FERBERUS_PYTHONS:-3.9 3.10 3.11 3.12 3.13 3.14 3.15 3.14t 3.15t}"
stages=("$@")
[ ${#stages[@]} -eq 0 ] && stages=(lint rust python bench)

for stage in "${stages[@]}"; do
    case "$stage" in
        lint)
            cargo fmt --all --check
            cargo clippy --workspace --all-targets --release -- -D warnings
            cargo deny check
            ruff check .
            ruff format --check .
            ;;
        rust)
            cargo nextest run --workspace --release --no-tests=pass
            scripts/fuzz.sh "${FERBERUS_FUZZ_SECONDS:-30}" >/dev/null
            ;;
        python)
            for v in $PYTHONS; do
                echo "::group::CPython $v"
                scripts/develop.sh "$v"
                scripts/upstream-tests.sh "$v"
                if "/opt/venvs/$v/bin/python" -c "import hypothesis" 2>/dev/null; then
                    HYPOTHESIS_PROFILE="${HYPOTHESIS_PROFILE:-ci}" \
                        "/opt/venvs/$v/bin/python" -m pytest tests -q -p no:cacheprovider
                fi
                echo "::endgroup::"
            done
            ;;
        bench)
            scripts/develop.sh 3.14
            python benches/run.py --rounds 5 --json /tmp/bench.json
            python benches/check_regression.py /tmp/bench.json benches/results/baseline.json
            ;;
        *)
            echo "unknown stage: $stage" >&2
            exit 2
            ;;
    esac
done
