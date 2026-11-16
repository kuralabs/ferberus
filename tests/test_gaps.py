"""
Behaviors the upstream suite does not pin down, checked against real
Cerberus: each test body runs once per implementation and the observable
outcomes must be identical.
"""

import copy
import pickle
import warnings
from collections import OrderedDict, UserDict
from datetime import date, datetime
from decimal import Decimal

import pytest
from ferberus import _core

from tests.differential.harness import _attempt, canon, canon_error, snapshot


def same(cerberus, ferberus, scenario):
    """Runs ``scenario(lib)`` on both libraries and compares what it returns."""
    results = []
    for lib in (cerberus, ferberus):
        lib.schema_registry.clear()
        lib.rules_set_registry.clear()
        lib.Validator.clear_caches()
        with warnings.catch_warnings(record=True) as caught:
            warnings.simplefilter("always")
            outcome = _attempt(lambda lib=lib: scenario(lib))
            if outcome[0] == "raised" and isinstance(outcome[2], str):
                # messages can name the module of a class
                outcome = outcome[:2] + (outcome[2].replace("ferberus", "cerberus"),)
            results.append((outcome, sorted((w.category.__name__, str(w.message)) for w in caught)))
    assert results[0] == results[1]
    return results[1][0]


# -- extension points ----------------------------------------------------------


def test_overridden_error_method(cerberus, ferberus):
    """A class that overrides engine internals uses the reference implementation."""

    def scenario(lib):
        class Shouting(lib.Validator):
            def _error(self, *args):
                if len(args) == 2 and isinstance(args[1], str):
                    args = (args[0], args[1].upper())
                super()._error(*args)

            def _check_with_odd(self, field, value):
                if not value & 1:
                    self._error(field, "must be odd")

        v = Shouting(
            {
                "a": {"check_with": "odd"},
                "b": {"type": "dict", "schema": {"c": {"check_with": "odd"}}},
            }
        )
        return v.validate({"a": 2, "b": {"c": 4}}), snapshot(v)

    same(cerberus, ferberus, scenario)
    assert _core.stats()["declined_exotic_class"] > 0


def test_overridden_get_child_validator(cerberus, ferberus):
    def scenario(lib):
        created = []

        class Tracking(lib.Validator):
            def _get_child_validator(self, *args, **kwargs):
                child = super()._get_child_validator(*args, **kwargs)
                created.append((child.document_path, child.schema_path))
                return child

        v = Tracking(
            {
                "a": {
                    "type": "list",
                    "schema": {"type": "dict", "schema": {"b": {"type": "integer"}}},
                }
            }
        )
        return v.validate({"a": [{"b": 1}, {"b": "x"}]}), canon(created), snapshot(v)

    same(cerberus, ferberus, scenario)


def test_rule_monkeypatched_on_the_class_after_use(cerberus, ferberus):
    def scenario(lib):
        class Patched(lib.Validator):
            pass

        v = Patched({"a": {"min": 5}})
        before = v.validate({"a": 1}), canon(v.errors)

        def lenient_min(self, min_value, field, value):
            """{'nullable': False}"""

        Patched._validate_min = lenient_min
        after = v.validate({"a": 1}), canon(v.errors)
        del Patched._validate_min
        restored = v.validate({"a": 1}), canon(v.errors)
        return before, after, restored

    outcome = same(cerberus, ferberus, scenario)
    assert [result for result, _ in outcome[1]] == [False, True, False]


def test_rule_monkeypatched_on_the_instance(cerberus, ferberus):
    def scenario(lib):
        v = lib.Validator({"a": {"min": 5}})
        v._validate_min = lambda min_value, field, value: None
        return v.validate({"a": 1}), canon(v.errors)

    outcome = same(cerberus, ferberus, scenario)
    assert outcome == ("ok", (True, ("dict", [])))


def test_types_mapping_mutated_in_place(cerberus, ferberus):
    def scenario(lib):
        class Typed(lib.Validator):
            types_mapping = lib.Validator.types_mapping.copy()

        v = Typed({"a": {"type": "integer"}})
        first = v.validate({"a": "x"})
        Typed.types_mapping["integer"] = lib.TypeDefinition("integer", (str,), ())
        second = v.validate({"a": "x"})
        return first, second

    assert same(cerberus, ferberus, scenario) == ("ok", (False, True))


def test_legacy_type_method_and_missing_handler(cerberus, ferberus):
    def scenario(lib):
        with warnings.catch_warnings():
            warnings.simplefilter("ignore")

            class Legacy(lib.Validator):
                def _validate_type_objectid(self, value):
                    return isinstance(value, str) and len(value) == 3

        v = Legacy({"a": {"type": "objectid"}, "b": {"type": ["objectid", "integer"]}})
        return [
            (v.validate(d), canon(v.errors))
            for d in ({"a": "abc", "b": 1}, {"a": "abcd", "b": "x"})
        ]

    same(cerberus, ferberus, scenario)


def test_hooks_see_child_validators(cerberus, ferberus):
    """What `self` is for a hook at a nested level."""

    def scenario(lib):
        seen = []

        class Inspecting(lib.Validator):
            def __init__(self, *args, **kwargs):
                self.context = kwargs.get("context")
                super().__init__(*args, **kwargs)

            def _check_with_inspect(self, field, value):
                seen.append(
                    canon(
                        (
                            field,
                            self.is_child,
                            self.document_path,
                            self.schema_path,
                            self.document,
                            self.root_document,
                            self.schema,
                            self.root_schema is not None,
                            self.context,
                            self._config.get("context"),
                            self.update,
                            self.allow_unknown,
                            self.root_allow_unknown,
                            self.require_all,
                            self.purge_unknown,
                            self._is_normalized,
                            type(self.error_handler).__name__,
                            sorted(k for k in self._config if k != "error_handler"),
                        )
                    )
                )

            def _normalize_coerce_inspect(self, value):
                seen.append(
                    canon(("coerce", self.document_path, self.schema_path, self._is_normalized))
                )
                return value

        schema = {
            "top": {"check_with": "inspect", "coerce": "inspect"},
            "nested": {
                "type": "dict",
                "allow_unknown": True,
                "schema": {"x": {"check_with": "inspect", "coerce": "inspect"}},
            },
            "items": {
                "type": "list",
                "schema": {"type": "dict", "schema": {"y": {"check_with": "inspect"}}},
            },
            "choice": {"anyof": [{"check_with": "inspect"}, {"type": "string"}]},
            "values": {
                "valuesrules": {"check_with": "inspect"},
                "keysrules": {"check_with": "inspect"},
            },
            "tuple": {"items": [{"check_with": "inspect"}, {"coerce": "inspect"}]},
        }
        v = Inspecting(schema, context="ctx", purge_unknown=True)
        document = {
            "top": 1,
            "nested": {"x": 2, "extra": 3},
            "items": [{"y": 4}, {"y": 5}],
            "choice": 6,
            "values": {"k": 7},
            "tuple": [8, 9],
        }
        return (
            v.validate(document),
            v.validate(document, update=True, normalize=False),
            seen,
            snapshot(v),
        )

    same(cerberus, ferberus, scenario)
    stats = _core.stats()
    assert stats["object_frames"] > 0


def test_custom_error_handler_sees_every_event(cerberus, ferberus):
    def scenario(lib):
        events = []

        class Recording(lib.errors.BaseErrorHandler):
            def __call__(self, errors):
                return [canon_error(e) for e in errors]

            def start(self, validator):
                events.append(("start", len(validator._errors), canon(validator.document)))

            def emit(self, error):
                events.append(("emit", canon_error(error)))

            def end(self, validator):
                events.append(("end", len(validator._errors)))

        v = lib.Validator(
            {
                "a": {"type": "integer"},
                "b": {"type": "list", "schema": {"type": "integer"}},
                "c": {"required": True},
            },
            error_handler=Recording,
        )
        return v.validate({"a": "x", "b": [1, "y"], "d": 1}), v.errors, events

    same(cerberus, ferberus, scenario)


def test_monkeypatched_stock_handler_is_respected(cerberus, ferberus):
    def scenario(lib):
        handler = lib.errors.BasicErrorHandler
        original = handler._format_message
        handler._format_message = lambda self, field, error: "patched %s" % (field,)
        try:
            v = lib.Validator({"a": {"type": "integer"}})
            v.validate({"a": "x"})
            return canon(v.errors)
        finally:
            handler._format_message = original

    assert same(cerberus, ferberus, scenario) == (
        "ok",
        ("dict", [(("str", "a"), ("list", [("str", "patched a")]))]),
    )


def test_custom_messages_and_unknown_codes(cerberus, ferberus):
    def scenario(lib):
        class Quiet(lib.errors.BasicErrorHandler):
            messages = {0x24: "bad type {constraint!r} for {field!r}: {value!r}"}

        v = lib.Validator(
            {"a": {"type": "integer", "min": 5}, "b": {"required": True}}, error_handler=Quiet
        )
        results = [
            (v.validate({"a": "x"}), canon(v.errors)),
            (v.validate({"a": 1, "b": 1}), canon(v.errors)),
        ]
        # a logic error whose children have codes without message
        v.schema = {"a": {"anyof": [{"min": 5}, {"type": "string"}]}}
        results.append((v.validate({"a": 1}), _attempt(lambda: canon(v.errors))))
        return results

    same(cerberus, ferberus, scenario)


# -- documents and values --------------------------------------------------------


@pytest.mark.parametrize("factory", [OrderedDict, UserDict, dict], ids=lambda f: f.__name__)
def test_mapping_types_as_documents(cerberus, ferberus, factory):
    def scenario(lib):
        v = lib.Validator(
            {
                "a": {"type": "dict", "schema": {"b": {"coerce": int}}},
                "c": {"type": "dict", "valuesrules": {"type": "integer"}},
                "d": {"rename": "e"},
            },
            purge_unknown=True,
        )
        document = factory(
            [("a", factory([("b", "1")])), ("c", factory([("x", 1)])), ("d", 2), ("junk", 3)]
        )
        result = v.validate(document)
        return (
            result,
            canon(v.document),
            type(v.document).__name__,
            type(v.document["a"]).__name__,
            type(v.document["c"]).__name__,
            canon(document),
            snapshot(v),
        )

    same(cerberus, ferberus, scenario)


def test_sequence_types(cerberus, ferberus):
    def scenario(lib):
        class MyList(list):
            pass

        v = lib.Validator(
            {
                "a": {"type": "list", "schema": {"coerce": int}},
                "b": {"type": "list", "items": [{"coerce": str}, {"type": "integer"}]},
                "c": {"type": "list", "schema": {"type": "integer"}},
            }
        )
        document = {"a": ("1", "2"), "b": MyList([1, 2]), "c": range(3)}
        result = _attempt(lambda: v.validate(document))
        return result, canon(v.document), type(v.document.get("b")).__name__, snapshot(v)

    same(cerberus, ferberus, scenario)


def test_value_semantics(cerberus, ferberus):
    """bool/int/float/Decimal/datetime comparisons, containment and hashing."""

    def scenario(lib):
        v = lib.Validator(
            {
                "i": {"type": "integer", "min": 1, "max": 3, "allowed": [1, 2.0, True]},
                "n": {"type": "number", "min": Decimal("1.5")},
                "f": {"type": "float", "max": 2**70},
                "d": {"type": "date", "min": date(2020, 1, 1)},
                "t": {"type": "datetime", "max": datetime(2020, 1, 1)},
                "s": {"type": "string", "min": "b", "forbidden": ["x"], "contains": "a"},
                "l": {"type": "list", "contains": [1, 2], "forbidden": [True], "minlength": True},
                "big": {"min": 2**80, "max": 2**90},
                "nan": {"min": 0.0, "max": 1.0},
            }
        )
        documents = [
            {
                "i": True,
                "n": True,
                "f": 2**71,
                "d": datetime(2019, 1, 1),
                "t": date(2021, 1, 1),
                "s": "a",
                "l": [1.0],
            },
            {
                "i": 2,
                "n": 2.5,
                "f": 1,
                "d": date(2021, 1, 1),
                "t": datetime(2019, 1, 1),
                "s": "xa",
                "l": [1, 2, 3],
            },
            {"i": 2.0, "n": Decimal("1.4"), "f": 1.5, "s": "c", "big": 2**85, "nan": float("nan")},
            {"i": 4, "big": 2**91, "nan": 2.0, "s": b"bytes", "l": "string"},
        ]
        return [(v.validate(d), canon(v.errors), canon(v._errors)) for d in documents]

    same(cerberus, ferberus, scenario)


def test_user_defined_types_and_abc_registration(cerberus, ferberus):
    def scenario(lib):
        from collections.abc import Mapping, Sequence

        class Vector:
            def __init__(self, *items):
                self.items = items

            def __len__(self):
                return len(self.items)

            def __getitem__(self, index):
                return self.items[index]

            def __iter__(self):
                return iter(self.items)

            def __repr__(self):
                return "Vector%r" % (self.items,)

        class Unregistered(Vector):
            pass

        Sequence.register(Vector)
        v = lib.Validator(
            {
                "v": {
                    "type": "list",
                    "minlength": 2,
                    "schema": {"type": "integer"},
                    "allowed": [1, 2, 3],
                },
                "m": {"type": "dict"},
            }
        )
        results = []
        for document in (
            {"v": Vector(1, 2)},
            {"v": Vector(1, "x", 9)},
            {"v": Vector(1)},
            {"m": Vector()},
        ):
            results.append(
                (_attempt(lambda d=document: v.validate(d)), _attempt(lambda: canon(v.errors)))
            )
        assert not isinstance(Vector(), Mapping)
        return results

    same(cerberus, ferberus, scenario)


def test_input_is_not_modified_except_like_upstream(cerberus, ferberus):
    def scenario(lib):
        v = lib.Validator(
            {
                "a": {"type": "dict", "keysrules": {"coerce": str}, "valuesrules": {"coerce": int}},
                "b": {"type": "list", "schema": {"coerce": int}},
                "c": {"type": "dict", "schema": {"d": {"default": 1}}},
            }
        )
        document = {"a": {1: "1", 2: "2"}, "b": ["1"], "c": {}}
        nested = (document["a"], document["b"], document["c"])
        v.validate(document)
        return (
            canon(document),
            canon(v.document),
            [v.document[k] is n for k, n in zip("abc", nested)],
            v.document is document,
        )

    same(cerberus, ferberus, scenario)


# -- validator life cycle ----------------------------------------------------------


def test_reuse_and_state_between_calls(cerberus, ferberus):
    def scenario(lib):
        v = lib.Validator({"a": {"type": "integer", "required": True}, "b": {"excludes": "a"}})
        states = []
        for call in (
            lambda: v.validate({"a": "x"}),
            lambda: v.validate({"a": 1}),
            lambda: v.validate(None),
            lambda: v.validate({"b": 1}, update=True),
            lambda: v.normalized({"a": 1, "b": 2}),
            lambda: v.validated({"a": 1}),
            lambda: v.validate({"a": 1}, {"a": {"type": "string"}}),
            lambda: v({"a": "s"}),
            lambda: v.validate({}, normalize=False),
        ):
            states.append((_attempt(lambda c=call: canon(c())), snapshot(v)))
        return states

    same(cerberus, ferberus, scenario)


def test_copy_and_pickle(cerberus, ferberus):
    def scenario(lib):
        v = lib.Validator(
            {"a": {"type": "integer"}, "b": {"type": "list", "schema": {"type": "string"}}}
        )
        v.validate({"a": "x", "b": [1]})
        results = []
        for clone in (copy.copy(v), copy.deepcopy(v), pickle.loads(pickle.dumps(v))):
            before = snapshot(clone)
            again = clone.validate({"a": 1, "b": ["s"]}), snapshot(clone)
            results.append((before, again))
        # the original is unaffected
        results.append(snapshot(v))
        error = v._errors[0]
        for clone in (copy.copy(error), copy.deepcopy(error), pickle.loads(pickle.dumps(error))):
            results.append((canon_error(clone), clone == error, hash(clone) == hash(error)))
        return results

    same(cerberus, ferberus, scenario)


def test_schema_object_shared_between_validators(cerberus, ferberus):
    def scenario(lib):
        class Other(lib.Validator):
            def _validate_min(self, min_value, field, value):
                """{'nullable': False}"""
                self._error(field, "custom min")

        first = lib.Validator({"a": {"min": 5}})
        second = Other(first.schema)
        third = lib.Validator(first.schema, allow_unknown=True)
        return [
            (v.validate({"a": 1, "b": 2}), canon(v.errors))
            for v in (first, second, third, first, second)
        ]

    same(cerberus, ferberus, scenario)


def test_schema_changes(cerberus, ferberus):
    def scenario(lib):
        raw = {"a": {"type": "integer", "allowed": [1, 2]}, "b": {"anyof": [{"type": "string"}]}}
        v = lib.Validator(raw)
        results = [v.validate({"a": 3, "b": 1})]
        raw["a"]["allowed"].append(3)  # in place, behind the validator's back
        results.append(v.validate({"a": 3, "b": 1}))
        raw["b"]["anyof"].append({"type": "integer"})
        results.append(v.validate({"a": 3, "b": 1}))
        v.schema["c"] = {"required": True}  # through the DefinitionSchema
        results.append(v.validate({"a": 3, "b": 1}))
        v.schema.update({"c": {"required": False}})
        results.append(v.validate({"a": 3, "b": 1}))
        del v.schema["a"]
        results.append(v.validate({"a": 3, "b": 1}))
        raw["b"]["tüpe"] = "x"  # invalid, noticed with the next normalization
        results.append(_attempt(lambda: v.validate({"b": 1}, normalize=False)))
        results.append(_attempt(lambda: v.validate({"b": 1})))
        del raw["b"]["tüpe"]
        results.append(_attempt(lambda: v.validate({"b": 1})))
        v.allow_unknown = {"type": "string"}
        results.append((v.validate({"zzz": 1}), canon(v.errors)))
        v.require_all = True
        v.purge_unknown = True
        results.append((v.validate({"zzz": 1}), canon(v.errors), canon(v.document)))
        return results, canon(raw)

    same(cerberus, ferberus, scenario)


def test_valid_schema_cache_contents(cerberus, ferberus):
    def scenario(lib):
        schema = {
            "a": {"type": "integer"},
            "b": {"type": "dict", "schema": {"c": {"anyof_type": ["string", "integer"]}}},
        }
        sizes = [len(lib.Validator._valid_schemas)]
        v = lib.Validator(schema)
        sizes.append(len(v._valid_schemas))
        for _ in range(3):
            v.validate({"a": 1})
            sizes.append(len(v._valid_schemas))
        v.validate({"a": 1}, normalize=False)
        sizes.append(len(v._valid_schemas))
        lib.Validator.clear_caches()
        sizes.append(len(v._valid_schemas))
        v.validate({"a": 1}, normalize=False)
        sizes.append(len(v._valid_schemas))
        v.validate({"a": 1})
        sizes.append(len(v._valid_schemas))
        return sizes

    same(cerberus, ferberus, scenario)


def test_registries_per_validator(cerberus, ferberus):
    def scenario(lib):
        from_module = lib.schema.RulesSetRegistry((("r", {"type": "integer"}),))
        schemas = lib.schema.SchemaRegistry({"s": {"x": "r"}})
        v = lib.Validator(
            {"a": "r", "b": {"schema": "s"}},
            rules_set_registry=from_module,
            schema_registry=schemas,
        )
        results = [(v.validate({"a": "x", "b": {"x": "y"}}), canon(v.errors))]
        from_module.add("r", {"type": "string"})
        results.append((v.validate({"a": "x", "b": {"x": "y"}}), canon(v.errors)))
        from_module.remove("r")
        results.append(_attempt(lambda: v.validate({"a": "x"})))
        return results

    same(cerberus, ferberus, scenario)


def test_deeply_nested_documents(cerberus, ferberus):
    def scenario(lib):
        schema = {"a": {"type": "integer"}}
        document = {"a": "x"}
        # (Much deeper and upstream runs into RecursionError while it files
        # the error, see COMPATIBILITY.md.)
        for _ in range(20):
            schema = {"n": {"type": "dict", "schema": schema}}
            document = {"n": document}
        v = lib.Validator(schema)
        return v.validate(document), canon(v.errors), canon(v._errors)

    same(cerberus, ferberus, scenario)


def test_native_engine_can_be_switched_off(cerberus, ferberus):
    schema = {"a": {"type": "list", "schema": {"coerce": int}}}
    v = ferberus.Validator(schema)
    assert v.validate({"a": ["1"]})
    expected = v.document
    previous = _core.set_enabled(False)
    try:
        _core.reset_stats()
        assert v.validate({"a": ["1"]}) and v.document == expected
        stats = _core.stats()
        assert stats["native_calls"] == 0 and stats["declined_disabled"] > 0
    finally:
        _core.set_enabled(previous)


def test_cerberus_alias(ferberus):
    import subprocess
    import sys

    code = (
        "import ferberus, ferberus.compat as c; c.install(); import cerberus, cerberus.errors;"
        "assert cerberus.Validator is ferberus.Validator;"
        "assert cerberus.errors is ferberus.errors;"
        "from cerberus.validator import BareValidator; from cerberus.schema import Registry;"
        "from cerberus.utils import validator_factory; from cerberus.platform import Mapping;"
        "assert cerberus.Validator({'a': {'type': 'string'}}).validate({'a': 'x'});"
        "c.uninstall(); import sys; assert 'cerberus' not in sys.modules"
    )
    subprocess.run([sys.executable, "-c", code], check=True)
