"""Run the upstream suite against ferberus and report engine statistics."""

import os
import sys

import pytest

UPSTREAM = os.environ.get("CERBERUS_CHECKOUT", "/opt/cerberus")
os.environ["FERBERUS_UPSTREAM_PACKAGE"] = UPSTREAM + "/cerberus"

import ferberus.compat  # noqa: E402
from ferberus import _core  # noqa: E402

ferberus.compat.install()
_core.reset_stats(True)
code = pytest.main(
    [
        "-p",
        "no:cacheprovider",
        "--rootdir",
        UPSTREAM,
        "-c",
        UPSTREAM + "/pyproject.toml",
        "-q",
        UPSTREAM + "/cerberus/tests",
    ]
    + sys.argv[1:]
)
stats = _core.stats()
reasons = stats.pop("reasons")
for key, value in stats.items():
    print("%-30s %d" % (key, value))
for key, value in sorted(reasons.items(), key=lambda kv: -kv[1]):
    print("    %-45s %d" % (key, value))
sys.exit(code)
