import os
import sys

import pytest

CERBERUS_CHECKOUT = os.environ.get("CERBERUS_CHECKOUT", "/opt/cerberus")

try:
    from hypothesis import HealthCheck, settings
except ImportError:  # interpreters without Hypothesis (see .devcontainer)
    settings = None

if settings is not None:
    _common = dict(
        deadline=None,
        suppress_health_check=[HealthCheck.too_slow, HealthCheck.data_too_large],
    )
    settings.register_profile("default", max_examples=300, **_common)
    settings.register_profile("ci", max_examples=2000, **_common)
    settings.register_profile("long", max_examples=100_000, **_common)
    settings.register_profile("soak", max_examples=2_000_000, **_common)
    if os.environ.get("HYPOTHESIS_MAX_EXAMPLES"):
        # ad-hoc session length: HYPOTHESIS_PROFILE=custom HYPOTHESIS_MAX_EXAMPLES=20000
        settings.register_profile(
            "custom", max_examples=int(os.environ["HYPOTHESIS_MAX_EXAMPLES"]), **_common
        )
    settings.load_profile(os.environ.get("HYPOTHESIS_PROFILE", "default"))


@pytest.fixture(scope="session")
def cerberus():
    """The real, pinned Cerberus (reference implementation)."""
    if not os.path.isdir(CERBERUS_CHECKOUT):
        pytest.skip("no Cerberus checkout at %s" % CERBERUS_CHECKOUT)
    if CERBERUS_CHECKOUT not in sys.path:
        sys.path.append(CERBERUS_CHECKOUT)
    import cerberus

    assert "ferberus" not in cerberus.__file__, "the ferberus alias shadows real Cerberus"
    return cerberus


@pytest.fixture(scope="session")
def ferberus():
    import ferberus

    return ferberus


def pytest_sessionstart(session):
    from ferberus import _core

    _core.reset_stats(True)


def pytest_terminal_summary(terminalreporter):
    from ferberus import _core

    stats = _core.stats()
    reasons = stats.pop("reasons")
    terminalreporter.section("ferberus engine statistics")
    for key, value in stats.items():
        terminalreporter.write_line("%-30s %d" % (key, value))
    for key, value in sorted(reasons.items(), key=lambda kv: -kv[1]):
        terminalreporter.write_line("    %-50s %d" % (key, value))
