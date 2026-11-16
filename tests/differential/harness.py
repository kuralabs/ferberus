"""
Differential harness: run one scenario against real Cerberus and ferberus and
capture everything observable in a comparable, implementation-neutral form.

A *scenario* is plain data (plus callables from ``POOL``), so a failing one
can be pasted into ``tests/regressions`` verbatim:

    Scenario(
        schema={...},                  # or a registry name
        calls=[Call('validate', document, {...kwargs})],
        config={...},                  # Validator(**config)
        registry={'schemas': {...}, 'rules_sets': {...}},
        validator='plain',             # key of VALIDATOR_FACTORIES
    )
"""

import copy
import re
import warnings
from collections import OrderedDict, UserDict
from dataclasses import dataclass, field
from datetime import date, datetime
from decimal import Decimal
from types import MappingProxyType

# --------------------------------------------------------------------------
# callables that may appear in schemas
# --------------------------------------------------------------------------


def _raise_value_error(value):
    raise ValueError("coercion refused for %r" % (value,))


def _raise_key_error(doc):
    raise KeyError("missing")


def _setter_from_a(doc):
    return doc["a"]


def _setter_from_b(doc):
    return doc["b"]


def _setter_const(doc):
    return 7


def _check_positive(field, value, error):
    if not isinstance(value, (int, float)) or value <= 0:
        error(field, "must be positive")


def _check_twice(field, value, error):
    error(field, "first complaint")
    error(field, "second complaint")


def _check_raises(field, value, error):
    raise RuntimeError("check exploded")


def _check_other_field(field, value, error):
    error("somewhere_else", "error for another field")


def _rename_upper(name):
    return name.upper() if isinstance(name, str) else name


def _rename_const(name):
    return "renamed"


def _to_list(value):
    return list(value)


def _to_dict_items(value):
    return dict(value)


def _strip(value):
    return value.strip()


POOL = {
    "int": int,
    "str": str,
    "float": float,
    "bool": bool,
    "strip": _strip,
    "to_list": _to_list,
    "to_dict": _to_dict_items,
    "raise_value_error": _raise_value_error,
    "raise_key_error": _raise_key_error,
    "setter_from_a": _setter_from_a,
    "setter_from_b": _setter_from_b,
    "setter_const": _setter_const,
    "check_positive": _check_positive,
    "check_twice": _check_twice,
    "check_raises": _check_raises,
    "check_other_field": _check_other_field,
    "rename_upper": _rename_upper,
    "rename_const": _rename_const,
}
_POOL_NAMES = {id(v): k for k, v in POOL.items()}


# --------------------------------------------------------------------------
# validator classes under test
# --------------------------------------------------------------------------


def _plain(lib):
    return lib.Validator


def _custom_rules(lib):
    class CustomValidator(lib.Validator):
        def _validate_is_even(self, constraint, field, value):
            """{'type': 'boolean'}"""
            if constraint and isinstance(value, int) and value % 2:
                self._error(field, "must be even")

        def _validate_stop(self, constraint, field, value):
            """{'type': 'boolean'}"""
            return constraint

        def _validate_drops(self, constraint, field, value):
            """{'type': 'list'}"""
            self._drop_remaining_rules(*constraint)

        def _check_with_short(self, field, value):
            if isinstance(value, (str, list)) and len(value) > 3:
                self._error(field, "too long")

        def _check_with_sees_document(self, field, value):
            if "a" in self.document and self.document["a"] == value:
                self._error(field, "equals a")
            if self.root_document is not self.document and field in self.root_document:
                self._error(field, "shadowed in root")

        def _normalize_coerce_upper(self, value):
            return value.upper()

        def _normalize_default_setter_path(self, document):
            return len(self.document_path)

    return CustomValidator


def _typed(lib):
    class TypedValidator(lib.Validator):
        types_mapping = lib.Validator.types_mapping.copy()
        types_mapping["decimal"] = lib.TypeDefinition("decimal", (Decimal,), ())
        types_mapping["scalar"] = lib.TypeDefinition("scalar", (int, float, str), (bool,))

    return TypedValidator


def _overrides_builtin(lib):
    class OverridingValidator(lib.Validator):
        def _validate_min(self, min_value, field, value):
            """{'nullable': False}"""
            if isinstance(value, (int, float)) and value < min_value:
                self._error(field, "below custom minimum")

    return OverridingValidator


def _context(lib):
    class ContextValidator(lib.Validator):
        def __init__(self, *args, **kwargs):
            self.limit = kwargs.get("limit", 2)
            super().__init__(*args, **kwargs)

        def _check_with_limit(self, field, value):
            if isinstance(value, int) and value > self.limit:
                self._error(field, "exceeds %s" % self._config.get("limit"))

    return ContextValidator


VALIDATOR_FACTORIES = {
    "plain": _plain,
    "custom_rules": _custom_rules,
    "typed": _typed,
    "overrides_builtin": _overrides_builtin,
    "context": _context,
}


# --------------------------------------------------------------------------
# scenario
# --------------------------------------------------------------------------


@dataclass
class Call:
    method: str  # validate | normalized | validated | call | set_schema
    document: object = None
    kwargs: dict = field(default_factory=dict)


@dataclass
class Scenario:
    schema: object = None
    calls: list = field(default_factory=list)
    config: dict = field(default_factory=dict)
    registry: dict = field(default_factory=dict)
    validator: str = "plain"
    handler: str = "basic"  # basic | emitting | custom_messages


def clone(obj):
    """Deep copy that keeps pool callables shared and supports mapping proxies."""
    t = type(obj)
    if t is MappingProxyType:
        return MappingProxyType({clone(k): clone(v) for k, v in obj.items()})
    if t in (dict, OrderedDict):
        return t((clone(k), clone(v)) for k, v in obj.items())
    if t is UserDict:
        return UserDict({clone(k): clone(v) for k, v in obj.items()})
    if t in (list, tuple, set, frozenset):
        return t(clone(x) for x in obj)
    return copy.deepcopy(obj)


# --------------------------------------------------------------------------
# canonical form of observable results
# --------------------------------------------------------------------------

_ADDRESS = re.compile(r"0x[0-9a-fA-F]+")


def canon(obj, depth=0):
    """Type-strict, order-preserving, address-free representation."""
    if depth > 40:
        return "<too deep>"
    t = type(obj)
    name = t.__name__
    if obj is None or t in (bool, int, str, bytes):
        return (name, obj)
    if t is float:
        return (name, repr(obj))
    if t in (list, tuple):
        return (name, [canon(x, depth + 1) for x in obj])
    if t in (dict, OrderedDict):
        return (name, [(canon(k, depth + 1), canon(v, depth + 1)) for k, v in obj.items()])
    if isinstance(obj, (UserDict, MappingProxyType)):
        return (name, [(canon(k, depth + 1), canon(v, depth + 1)) for k, v in obj.items()])
    if t in (set, frozenset):
        return (name, [canon(x, depth + 1) for x in obj])
    if t in (datetime, date, Decimal, bytearray):
        return (name, repr(obj))
    if id(obj) in _POOL_NAMES:
        return ("callable", _POOL_NAMES[id(obj)])
    module = getattr(t, "__module__", "")
    if name == "ValidationError":
        return canon_error(obj, depth + 1)
    if name == "ErrorList":
        return ("ErrorList", [canon(x, depth + 1) for x in obj])
    if name in ("ErrorDefinition", "TypeDefinition"):
        return (name, [canon(x, depth + 1) for x in obj])
    if name in ("DefinitionSchema", "UnvalidatedSchema"):
        return (name, canon(obj.schema, depth + 1))
    if isinstance(obj, type):
        return ("type", obj.__name__)
    if module.split(".")[0] in ("cerberus", "ferberus"):
        return ("lib-object", name)
    return (name, _ADDRESS.sub("0x", repr(obj)))


def canon_error(error, depth=0):
    return (
        "ValidationError",
        canon(error.document_path, depth + 1),
        canon(error.schema_path, depth + 1),
        error.code,
        error.rule,
        canon(error.constraint, depth + 1),
        canon(error.value, depth + 1),
        canon(error.info, depth + 1),
    )


def canon_tree(node, depth=0):
    if node is None:
        return None
    if depth > 40:
        return "<too deep>"
    return (
        type(node).__name__,
        canon(node.path),
        [canon_error(e) for e in node.errors],
        [(canon(k), canon_tree(v, depth + 1)) for k, v in node.descendants.items()],
    )


def canon_exception(exc):
    args = exc.args
    if type(exc).__name__ == "SchemaError" and args and not isinstance(args[0], str):
        rendered = canon(args[0])
    else:
        rendered = _ADDRESS.sub("0x", str(exc))
    return ("raised", type(exc).__name__, rendered)


def _attempt(fn):
    try:
        return ("ok", fn())
    except RecursionError:
        raise
    except Exception as exc:  # noqa: BLE001 - everything is part of the surface
        return canon_exception(exc)


def snapshot(validator):
    """Everything observable on a validator after a call."""
    state = {
        "document": _attempt(lambda: canon(validator.document)),
        "_errors": _attempt(lambda: canon(validator._errors)),
        "errors": _attempt(lambda: canon(validator.errors)),
        "recent_error": _attempt(lambda: canon(validator.recent_error)),
        "document_error_tree": _attempt(lambda: canon_tree(validator.document_error_tree)),
        "schema_error_tree": _attempt(lambda: canon_tree(validator.schema_error_tree)),
        "update": validator.update,
        "_remaining_rules": canon(validator._remaining_rules),
        "_unrequired_by_excludes": _attempt(
            lambda: sorted(repr(x) for x in validator._unrequired_by_excludes)
        ),
        "_is_normalized": validator._is_normalized,
        "schema": _attempt(lambda: canon(validator.schema)),
        "handler": _attempt(lambda: canon(getattr(validator.error_handler, "seen", None))),
        "handler_tree": _attempt(lambda: canon(getattr(validator.error_handler, "tree", None))),
    }
    return state


def _make_handler(lib, kind):
    errors = lib.errors
    if kind == "basic":
        return {}
    if kind == "emitting":

        class EmittingHandler(errors.BasicErrorHandler):
            def __init__(self, *args, **kwargs):
                super().__init__(*args, **kwargs)
                self.seen = []

            def start(self, validator):
                super().start(validator)
                self.seen.append(("start", len(validator._errors)))

            def emit(self, error):
                self.seen.append(("emit", canon_error(error)))

            def end(self, validator):
                self.seen.append(("end", len(validator._errors)))

        return {"error_handler": EmittingHandler}
    if kind == "custom_messages":

        class CustomMessages(errors.BasicErrorHandler):
            messages = errors.BasicErrorHandler.messages.copy()
            messages[0x24] = "wrong type, wanted {constraint}, got {value!r}"
            messages[0x02] = "{field} is mandatory"

        return {"error_handler": CustomMessages()}
    raise ValueError(kind)


def run(lib, scenario):
    """Execute a scenario against ``lib``; returns a list of canonical records."""
    records = []
    lib.schema_registry.clear()
    lib.rules_set_registry.clear()
    lib.Validator.clear_caches()

    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")

        def consume_warnings():
            seen = [(w.category.__name__, _ADDRESS.sub("0x", str(w.message))) for w in caught]
            del caught[:]
            return seen

        registry = clone(scenario.registry)
        setup = _attempt(
            lambda: (
                (
                    lib.schema_registry.extend(registry.get("schemas", {})),
                    lib.rules_set_registry.extend(registry.get("rules_sets", {})),
                )
                and None
            )
        )
        records.append(("registry", setup, consume_warnings()))

        schema = clone(scenario.schema)
        config = clone(scenario.config)
        holder = {}

        def construct():
            cls = VALIDATOR_FACTORIES[scenario.validator](lib)
            kwargs = dict(config)
            kwargs.update(_make_handler(lib, scenario.handler))
            holder["v"] = cls(schema, **kwargs)
            return None

        outcome = _attempt(construct)
        records.append(("construct", outcome, canon(schema), consume_warnings()))
        validator = holder.get("v")
        if validator is None:
            return records

        for call in scenario.calls:
            document = clone(call.document)
            kwargs = clone(call.kwargs)
            if call.method == "set_schema":
                outcome = _attempt(lambda: setattr(validator, "schema", document))
            elif call.method == "mutate_schema":
                # mutation of the live schema through the DefinitionSchema API
                # (top level) or in place, behind the validator's back (nested)
                outcome = _attempt(lambda: _mutate(validator.schema, document))
            elif call.method == "mutate_raw":
                outcome = _attempt(lambda: _mutate(validator.schema.schema, document))
            elif call.method == "registry_add":
                kind, name, definition = document
                registry = lib.schema_registry if kind == "schemas" else lib.rules_set_registry
                outcome = _attempt(lambda: registry.add(name, definition))
            elif call.method == "registry_remove":
                kind, name = document
                registry = lib.schema_registry if kind == "schemas" else lib.rules_set_registry
                outcome = _attempt(lambda: registry.remove(name))
            elif call.method == "clear_caches":
                outcome = _attempt(lambda: type(validator).clear_caches())
            else:
                method = validator if call.method == "call" else getattr(validator, call.method)
                outcome = _attempt(lambda: canon(method(document, **kwargs)))
            records.append(
                (
                    call.method,
                    outcome,
                    canon(document),  # the caller's input after the call
                    snapshot(validator),
                    consume_warnings(),
                )
            )
    return records


def _mutate(schema, instructions):
    """``instructions``: list of (path, value) – assign ``value`` at ``path``."""
    for path, value in instructions:
        target = schema
        for key in path[:-1]:
            target = target[key]
        if isinstance(value, str) and value == "<delete>":
            del target[path[-1]]
        elif isinstance(value, tuple) and value and value[0] == "<append>":
            target[path[-1]].append(value[1])
        else:
            target[path[-1]] = value


def first_difference(a, b, path="result"):
    """Human-readable location of the first difference between two records."""
    if type(a) is not type(b):
        return "%s: type %s != %s\n  cerberus: %r\n  ferberus: %r" % (
            path,
            type(a).__name__,
            type(b).__name__,
            a,
            b,
        )
    if isinstance(a, dict):
        for key in a:
            if key not in b:
                return "%s: key %r missing in ferberus" % (path, key)
            d = first_difference(a[key], b[key], "%s[%r]" % (path, key))
            if d:
                return d
        for key in b:
            if key not in a:
                return "%s: extra key %r in ferberus" % (path, key)
        return None
    if isinstance(a, (list, tuple)):
        for i, (x, y) in enumerate(zip(a, b)):
            d = first_difference(x, y, "%s[%d]" % (path, i))
            if d:
                return d
        if len(a) != len(b):
            return "%s: length %d != %d\n  cerberus: %r\n  ferberus: %r" % (
                path,
                len(a),
                len(b),
                a,
                b,
            )
        return None
    if a != b:
        return "%s:\n  cerberus: %r\n  ferberus: %r" % (path, a, b)
    return None


def compare(cerberus, ferberus, scenario):
    """Run ``scenario`` on both; returns ``None`` or a description of the mismatch."""
    expected = run(cerberus, scenario)
    actual = run(ferberus, scenario)
    return first_difference(expected, actual)
