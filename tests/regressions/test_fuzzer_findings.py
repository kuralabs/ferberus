"""
Every discrepancy the differential fuzzer found, kept as a permanent test.

Each scenario is replayed against real Cerberus and ferberus (see
``tests/differential/harness.py``); the comment says what went wrong.
"""

import pytest

from tests.differential.harness import POOL, Call, Scenario, compare

SCENARIOS = {
    # An exception inside a rule leaves the field's unprocessed rules in
    # ``_remaining_rules`` (here: TypeError while sorting errors with the
    # unorderable paths ('a',) and (None,)).
    "remaining_rules_after_exception": Scenario(
        schema={"a": {}, None: {"minlength": 0}},
        calls=[Call("validate", {"a": None, None: None})],
    ),
    # _validate_min files its error inside `try/except TypeError`: the
    # TypeError from sorting is swallowed.
    "min_swallows_type_error_of_error_filing": Scenario(
        schema={"a": {}, None: {"min": 1}},
        calls=[Call("validate", {"a": None, None: 0})],
    ),
    "max_swallows_type_error_of_error_filing": Scenario(
        schema={"a": {}, None: {"max": 1}},
        calls=[Call("validate", {"a": None, None: 5})],
    ),
    # A field named None is "no crumb" for its child validator.
    "none_field_does_not_extend_document_path": Scenario(
        schema={None: {"schema": {"schema": {}}}},
        calls=[Call("validate", {None: {"schema": None}})],
    ),
    # Adding to the error trees raises although sorting `_errors` did not.
    "error_tree_raises_for_unorderable_paths": Scenario(
        schema={"a": {}, "b": {"nullable": False, "schema": {}}},
        calls=[Call("validate", {"a": None, "b": {None: None, "a": None}})],
        validator="custom_rules",
    ),
    # Nested *of definitions inherit the outer definition's allow_unknown.
    "nested_logical_inherits_allow_unknown": Scenario(
        schema={
            "a": {
                "nullable": False,
                "min": 0,
                "noneof": [{"nullable": False, "oneof_schema": [{}], "min": 0}],
            }
        },
        calls=[
            Call("validate", None),
            Call("validate", {"a": 0}),
            Call("validate", {"a": {"a": None}}),
        ],
    ),
    # Tuple field names are spliced into the document path of children.
    "tuple_field_is_spliced_into_document_path": Scenario(
        schema={("t", 1): {"schema": {"a": {"type": "integer"}}}},
        calls=[Call("validate", {("t", 1): {"a": "x"}})],
    ),
    # Hooks at nested levels see a real child validator.
    "nested_check_with_method": Scenario(
        schema={"a": {"type": "integer"}, "b": {"schema": {"a": {"check_with": "sees_document"}}}},
        calls=[Call("validate", {"a": 1, "b": {"a": 1}})],
        validator="custom_rules",
    ),
    # A failing coercer of the validator's allow_unknown rules set raises
    # KeyError while filing the error (upstream bug, preserved).
    "allow_unknown_coercion_failure": Scenario(
        schema={"a": {"type": "integer"}},
        calls=[Call("validate", {"a": 1, "b": "x"}), Call("normalized", {"b": "1"})],
        config={"allow_unknown": {"coerce": POOL["int"]}},
    ),
    # `empty` drops rules *before* it files its error; if filing raises, the
    # rules stay dropped (queue kept on the validator because of check_with).
    "drops_take_effect_before_a_failing_error": Scenario(
        schema={
            "a": {"type": "binary"},
            "b": {},
            None: {"empty": False, "type": "binary", "check_with": POOL["check_positive"]},
        },
        calls=[
            Call("validate", {"a": b"", "b": None, None: b""}),
            Call("validate", {"a": b"", "b": None, None: None}),
        ],
    ),
    # BasicErrorHandler formats a deep copy of each error; a copied set can
    # iterate (and print) in another order than the original.
    "error_message_shows_a_deep_copied_set": Scenario(
        schema={"a": {}, "b": {"contains": ["a", "abc"]}},
        calls=[Call("validate", {"a": None, "b": []})],
    ),
    # The normalized value is assigned to the field before the errors of the
    # child are filed; filing raises here (unorderable error paths).
    "normalized_container_is_assigned_before_errors_are_filed": Scenario(
        schema={
            "a": {"readonly": True, "type": "binary"},
            None: {
                "valuesrules": {
                    "empty": False,
                    "default_setter": POOL["setter_from_a"],
                    "nullable": False,
                    "rename": "a",
                },
                "type": "binary",
            },
        },
        calls=[Call("validate", {"a": b"", None: {"a": None}})],
    ),
    # rename + rename_handler on a field named None: KeyError(None), whose
    # message is 'None' (and not empty).
    "key_error_for_a_field_named_none": Scenario(
        schema={"d": {"valuesrules": {"rename": "a", "rename_handler": POOL["rename_const"]}}},
        calls=[Call("validate", {"d": {None: None}})],
    ),
}


@pytest.mark.parametrize("name", sorted(SCENARIOS))
def test_fuzzer_finding(cerberus, ferberus, name):
    difference = compare(cerberus, ferberus, SCENARIOS[name])
    assert difference is None, difference
