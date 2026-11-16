"""
Differential property tests: real Cerberus vs ferberus.

Profiles (``HYPOTHESIS_PROFILE``): default (quick), ci, long, soak.
"""

import pytest

hypothesis = pytest.importorskip("hypothesis")

from hypothesis import given  # noqa: E402

from . import strategies as S  # noqa: E402
from .harness import compare  # noqa: E402


def check(cerberus, ferberus, scenario):
    try:
        difference = compare(cerberus, ferberus, scenario)
    except RecursionError:
        # Recursive registry definitions exhaust the stack in both
        # implementations; where exactly is not part of the contract.
        hypothesis.reject()
    assert difference is None, "%s\n\nscenario: %r" % (difference, scenario)


@given(scenario=S.scenarios())
def test_plain_validator(cerberus, ferberus, scenario):
    check(cerberus, ferberus, scenario)


@given(scenario=S.scenarios(handlers=("basic", "emitting", "custom_messages"), junk=False))
def test_error_handlers(cerberus, ferberus, scenario):
    check(cerberus, ferberus, scenario)


@given(
    scenario=S.scenarios(
        validators=("custom_rules", "typed", "overrides_builtin", "context"), junk=False
    )
)
def test_subclassed_validators(cerberus, ferberus, scenario):
    check(cerberus, ferberus, scenario)


@given(scenario=S.registry_scenarios())
def test_registries(cerberus, ferberus, scenario):
    check(cerberus, ferberus, scenario)


@given(scenario=S.mutation_scenarios(validators=("plain", "custom_rules")))
def test_schema_mutation(cerberus, ferberus, scenario):
    check(cerberus, ferberus, scenario)
