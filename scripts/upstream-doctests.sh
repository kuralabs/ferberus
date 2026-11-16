#!/usr/bin/env bash
# Run the doctests of Cerberus' documentation against ferberus (upstream:
# `tox -e doctest`). The docs are copied, their build writes into the tree.
set -euo pipefail
UPSTREAM="${CERBERUS_CHECKOUT:-/opt/cerberus}"
work="$(mktemp -d /tmp/cerberus-docs.XXXXXX)"
cp -r "$UPSTREAM/docs" "$work/docs"
# docs/includes/generate.py reads error definitions from the source tree
cp -r "$UPSTREAM/cerberus" "$work/cerberus"
cd "$work/docs"
python - "$@" <<'PY'
import sys

import ferberus.compat

ferberus.compat.install()

from sphinx.cmd.build import main

sys.exit(main(["-b", "doctest", "-q", ".", "_build/doctest"] + sys.argv[1:]))
PY
tail -n 12 _build/doctest/output.txt
