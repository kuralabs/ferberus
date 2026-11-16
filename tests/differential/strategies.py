"""
Hypothesis strategies generating schemas, documents and call sequences.

The generators are biased towards *valid* schemas and documents that roughly
fit them (otherwise nearly every example would end in a type error at the
top level), while keeping a steady share of mismatches, odd keys, odd
containers and outright invalid schemas.
"""

from collections import OrderedDict, UserDict
from datetime import date, datetime
from decimal import Decimal
from types import MappingProxyType

from hypothesis import strategies as st

from .harness import POOL, Call, Scenario

# -- atoms -------------------------------------------------------------------

FIELD_NAMES = ["a", "b", "c", "d", "e", "somewhere_else"]
field_names = st.sampled_from(FIELD_NAMES)
odd_keys = st.sampled_from([0, 1, 2, True, None, 1.5, ("t", 1), "a.b", "^a", "", "A"])
keys = st.one_of(field_names, field_names, field_names, field_names, odd_keys)

small_ints = st.integers(min_value=-3, max_value=12)
strings = st.sampled_from(
    [
        "",
        "a",
        "b",
        "abc",
        "abcdef",
        "ABC",
        " padded ",
        "12",
        "1.5",
        "x@y.zz",
        "foo\n",
        "ünï",
        "2020-01-01",
    ]
)
scalars = st.one_of(
    st.none(),
    st.booleans(),
    small_ints,
    st.sampled_from([0.0, 1.0, 1.5, -2.5, 10.0, float("inf")]),
    strings,
    st.sampled_from([b"", b"ab", b"abcdef"]),
    st.sampled_from([date(2020, 1, 2), datetime(2020, 1, 2, 3, 4, 5), Decimal("1.5"), Decimal(3)]),
)
hashable_scalars = st.one_of(st.none(), st.booleans(), small_ints, strings)


def containers(children):
    return st.one_of(
        st.lists(children, max_size=4),
        st.lists(children, max_size=3).map(tuple),
        st.dictionaries(keys, children, max_size=4),
        st.lists(hashable_scalars, max_size=3).map(set),
        st.lists(hashable_scalars, max_size=3).map(frozenset),
    )


values = st.recursive(scalars, containers, max_leaves=6)

TYPE_NAMES = [
    "binary",
    "boolean",
    "container",
    "date",
    "datetime",
    "dict",
    "float",
    "integer",
    "list",
    "number",
    "set",
    "string",
]

REGEXES = [
    "[a-z]+",
    "^[a-z]*$",
    "a|b",
    r"\d+",
    r"\w+@\w+\.\w{2,}",
    "(?i)abc",
    r"(a)\1",
    "abc$",
    r"abc\$",
    ".*",
    "",
    r"[^\s]+",
    r"\s*\S+\s*",
    "a{2,3}",
    "ab(",  # invalid: raises re.error at validation time
    r"(?=a)\w+",
    r"\d{4}-\d{2}-\d{2}",
    "ü+",
]

COERCERS = ["int", "str", "float", "bool", "strip", "to_list", "raise_value_error"]
SETTERS = ["setter_from_a", "setter_from_b", "setter_const", "raise_key_error", "raise_value_error"]
CHECKS = ["check_positive", "check_twice", "check_raises", "check_other_field"]
RENAMERS = ["rename_upper", "rename_const", "str", "raise_value_error"]


def one_in(n):
    """True with probability 1/n; shrinks towards False."""
    return st.sampled_from([False] * (n - 1) + [True])


def pool(names):
    return st.sampled_from(names).map(POOL.__getitem__)


def one_or_some(strategy, max_size=3):
    return st.one_of(strategy, st.lists(strategy, max_size=max_size))


# -- rules -------------------------------------------------------------------


def dependencies():
    paths = st.one_of(
        field_names, st.sampled_from(["a.b", "^a", "^a.b", "^^a", "b.c.d", "missing"])
    )
    return st.one_of(
        paths,
        st.lists(paths, max_size=3),
        st.dictionaries(paths, st.one_of(scalars, st.lists(scalars, max_size=3)), max_size=3),
        odd_keys,
    )


RULES_REFS = st.sampled_from(["r1", "r2", "r3", "missing"])
SCHEMA_REFS = st.sampled_from(["s1", "s2", "missing"])


def rules_sets(depth, validator="plain", normalization=True, refs=False):
    """A rules set (the value of a field in a schema)."""
    leaf = depth >= 3
    nested = st.deferred(lambda: rules_sets(depth + 1, validator, normalization, refs))
    nested_no_norm = st.deferred(lambda: rules_sets(depth + 1, validator, False, refs))
    nested_schema = st.deferred(lambda: mapping_schemas(depth + 1, validator, normalization, refs))
    if refs:
        # registry references wherever a rules set or schema is expected
        nested = st.one_of(nested, nested, RULES_REFS)
        nested_schema = st.one_of(nested_schema, SCHEMA_REFS, RULES_REFS)

    type_names = list(TYPE_NAMES)
    if validator == "typed":
        type_names += ["decimal", "scalar"]

    options = {
        "type": st.one_of(
            st.sampled_from(type_names),
            st.lists(st.sampled_from(type_names), min_size=1, max_size=3),
        ),
        "nullable": st.booleans(),
        "required": st.booleans(),
        "readonly": st.booleans(),
        "empty": st.booleans(),
        "allowed": st.one_of(
            st.lists(scalars, max_size=4),
            st.lists(hashable_scalars, max_size=4).map(set),
            st.sampled_from(["abc", ("a", "b")]),
        ),
        "forbidden": st.lists(hashable_scalars, max_size=4),
        "contains": st.one_of(hashable_scalars, st.lists(hashable_scalars, min_size=1, max_size=3)),
        "min": st.one_of(small_ints, st.sampled_from([0.5, "b", date(2020, 1, 1)])),
        "max": st.one_of(small_ints, st.sampled_from([0.5, "b", date(2020, 1, 1)])),
        "minlength": st.integers(min_value=0, max_value=4),
        "maxlength": st.integers(min_value=0, max_value=4),
        "regex": st.sampled_from(REGEXES),
        "dependencies": dependencies(),
        "excludes": st.one_of(field_names, st.lists(field_names, max_size=3)),
        "meta": values,
        "allow_unknown": st.one_of(st.booleans(), nested if not leaf else st.booleans()),
        "require_all": st.booleans(),
        "check_with": one_or_some(pool(CHECKS)),
    }
    if not leaf:
        options.update(
            {
                "items": st.lists(nested, max_size=3),
                "schema": st.one_of(nested, nested_schema, nested_schema),
                "keysrules": st.deferred(lambda: rules_sets(3, validator, normalization)),
                "valuesrules": nested,
                "anyof": st.lists(nested_no_norm, min_size=1, max_size=3),
                "allof": st.lists(nested_no_norm, min_size=1, max_size=3),
                "oneof": st.lists(nested_no_norm, min_size=1, max_size=3),
                "noneof": st.lists(nested_no_norm, min_size=1, max_size=3),
                "anyof_type": st.lists(st.sampled_from(type_names), min_size=1, max_size=3),
                "oneof_schema": st.lists(nested_schema, min_size=1, max_size=2),
            }
        )
    if normalization:
        options.update(
            {
                "coerce": one_or_some(pool(COERCERS)),
                "default": values,
                "default_setter": pool(SETTERS),
                "rename": keys,
                "rename_handler": one_or_some(pool(RENAMERS), 2),
                "purge_unknown": st.booleans(),
            }
        )
    if validator == "custom_rules":
        options.update(
            {
                "is_even": st.booleans(),
                "stop": st.booleans(),
                "drops": st.lists(
                    st.sampled_from(["min", "max", "type", "regex", "allowed"]), max_size=3
                ),
                "check_with": one_or_some(
                    st.one_of(
                        pool(CHECKS), st.sampled_from(["short", "sees_document", "child_validator"])
                    )
                ),
            }
        )
        if normalization:
            options["coerce"] = one_or_some(st.one_of(pool(COERCERS), st.just("upper")))
            options["default_setter"] = st.one_of(pool(SETTERS), st.just("path"))
    if validator == "context":
        options["check_with"] = one_or_some(st.one_of(pool(CHECKS), st.just("limit")))

    # weights: the bread-and-butter rules show up far more often
    common = ["type", "type", "type", "nullable", "required", "min", "max", "allowed", "regex"]
    common += ["minlength", "maxlength", "empty", "schema", "schema", "items", "valuesrules"]
    names = [n for n in common if n in options] + sorted(options)

    @st.composite
    def build(draw):
        chosen = draw(st.lists(st.sampled_from(names), max_size=5, unique=True))
        return {name: draw(options[name]) for name in chosen}

    return build()


def mapping_schemas(depth, validator="plain", normalization=True, refs=False):
    """A schema: field -> rules set."""
    definitions = rules_sets(depth, validator, normalization, refs)
    if refs:
        definitions = st.one_of(definitions, definitions, definitions, RULES_REFS)
    return st.dictionaries(keys, definitions, min_size=0 if depth else 1, max_size=4)


@st.composite
def registries(draw, validator="plain"):
    """Registry contents; definitions may refer to each other and themselves."""
    rules = rules_sets(1, validator, True, True)
    schemas = mapping_schemas(1, validator, True, True)
    return {
        "rules_sets": draw(st.dictionaries(st.sampled_from(["r1", "r2", "r3"]), rules, max_size=3)),
        "schemas": draw(st.dictionaries(st.sampled_from(["s1", "s2"]), schemas, max_size=2)),
    }


@st.composite
def schema_mutations(draw, schema, validator="plain"):
    """Instructions for ``harness._mutate``: changes to a live schema."""
    fields = [f for f, rules in schema.items() if isinstance(rules, dict)]
    if not fields:
        return [((draw(field_names),), draw(rules_sets(1, validator)))]
    field = draw(st.sampled_from(fields))
    rules = schema[field]
    choices = [
        st.tuples(st.just((field, "required")), st.booleans()),
        st.tuples(st.just((field, "nullable")), st.booleans()),
        st.tuples(st.just((field, "min")), small_ints),
        st.tuples(st.just((field, "type")), st.sampled_from(TYPE_NAMES)),
        st.tuples(st.just((field, "allowed")), st.lists(scalars, max_size=3)),
        st.tuples(st.just((field, "anyof")), st.lists(rules_sets(2, validator, False), max_size=2)),
        st.tuples(st.just((field,)), rules_sets(1, validator)),
        st.tuples(st.just((draw(field_names),)), rules_sets(1, validator)),
        st.tuples(st.just((field, "tüpe")), st.just("unknown rule")),
        st.tuples(st.just((field, "type")), st.just("no such type")),
    ]
    for rule, value in rules.items():
        if isinstance(value, list):
            choices.append(
                st.tuples(st.just((field, rule)), st.tuples(st.just("<append>"), scalars))
            )
            if value and isinstance(value[0], dict):
                choices.append(st.tuples(st.just((field, rule, 0, "min")), small_ints))
        if isinstance(value, dict) and value:
            choices.append(st.tuples(st.just((field, rule, "min")), small_ints))
            choices.append(st.tuples(st.just((field, rule, "type")), st.just("string")))
        choices.append(st.tuples(st.just((field, rule)), st.just("<delete>")))
    return draw(st.lists(st.one_of(*choices), min_size=1, max_size=2))


junk_schemas = st.one_of(
    st.dictionaries(keys, values, max_size=3),
    st.dictionaries(
        keys,
        st.dictionaries(
            st.sampled_from(
                [
                    "type",
                    "tüpe",
                    "min",
                    "schema",
                    "items",
                    "anyof",
                    "allowed",
                    "coerce",
                    "default setter",
                    "check with",
                ]
            ),
            values,
            max_size=3,
        ),
        max_size=3,
    ),
    values,
)

# -- documents ---------------------------------------------------------------

_TYPE_VALUES = {
    "binary": st.sampled_from([b"", b"ab", bytearray(b"abc")]),
    "boolean": st.booleans(),
    "container": st.one_of(
        st.lists(scalars, max_size=3), st.dictionaries(keys, scalars, max_size=2)
    ),
    "date": st.sampled_from([date(2020, 1, 2), datetime(2021, 1, 2)]),
    "datetime": st.just(datetime(2020, 1, 2, 3, 4)),
    "dict": st.dictionaries(keys, scalars, max_size=3),
    "float": st.sampled_from([0.0, 1.5, 3, -2.5]),
    "integer": st.one_of(small_ints, st.booleans()),
    "list": st.one_of(st.lists(scalars, max_size=4), st.lists(scalars, max_size=3).map(tuple)),
    "number": st.one_of(small_ints, st.sampled_from([1.5, -0.5])),
    "set": st.lists(hashable_scalars, max_size=3).map(set),
    "string": strings,
    "decimal": st.sampled_from([Decimal("1.5"), Decimal(2)]),
    "scalar": st.one_of(small_ints, strings),
}


def _looks_like_rules(mapping):
    return isinstance(mapping, dict) and not all(isinstance(v, dict) for v in mapping.values())


@st.composite
def value_for(draw, rules, depth=0):
    """A value that tends to satisfy ``rules``."""
    if not isinstance(rules, dict) or depth > 5 or draw(one_in(10)):
        return draw(values)
    if draw(one_in(15)):
        return None
    schema = rules.get("schema")
    type_ = rules.get("type")
    types = [type_] if isinstance(type_, str) else list(type_ or [])
    if isinstance(schema, dict) and schema and "list" not in types:
        if not _looks_like_rules(schema) or "dict" in types:
            return draw(document_for(schema, depth + 1))
    if isinstance(schema, dict) and ("list" in types or _looks_like_rules(schema)):
        return draw(st.lists(value_for(schema, depth + 1), max_size=3))
    if isinstance(rules.get("items"), list):
        items = [draw(value_for(r, depth + 1)) for r in rules["items"]]
        if draw(one_in(6)):
            items = items[:-1] if items else [1]
        return items
    for of in ("anyof", "oneof", "allof"):
        if isinstance(rules.get(of), list) and rules[of]:
            return draw(value_for(draw(st.sampled_from(rules[of])), depth + 1))
    if isinstance(rules.get("valuesrules"), dict) or isinstance(rules.get("keysrules"), dict):
        vr = rules.get("valuesrules", {})
        return draw(st.dictionaries(keys, value_for(vr, depth + 1), max_size=3))
    allowed = rules.get("allowed")
    if isinstance(allowed, (list, set)) and allowed and draw(st.booleans()):
        return draw(st.sampled_from(sorted(allowed, key=repr)))
    if types:
        name = draw(st.sampled_from(types))
        if name in _TYPE_VALUES:
            return draw(_TYPE_VALUES[name])
    if "regex" in rules:
        return draw(strings)
    if "min" in rules or "max" in rules:
        return draw(small_ints)
    return draw(values)


@st.composite
def document_for(draw, schema, depth=0):
    """A mapping that tends to fit ``schema``."""
    if not isinstance(schema, dict):
        return draw(st.dictionaries(keys, values, max_size=3))
    document = {}
    for name, rules in schema.items():
        if draw(one_in(5)):
            continue
        try:
            hash(name)
        except TypeError:
            continue
        document[name] = draw(value_for(rules, depth))
    extra = draw(st.dictionaries(keys, values, max_size=2)) if draw(one_in(4)) else {}
    for key, value in extra.items():
        document.setdefault(key, value)
    if draw(one_in(8)):
        # shuffle key order: order is observable (error order, normalization)
        items = draw(st.permutations(list(document.items())))
        document = dict(items)
    return document


def odd_document(document, kind):
    if kind == "ordered":
        return OrderedDict(document)
    if kind == "userdict":
        return UserDict(document)
    if kind == "proxy":
        return MappingProxyType(document)
    return document


# -- scenarios ---------------------------------------------------------------


@st.composite
def configs(draw, validator):
    config = {}
    if draw(one_in(4)):
        config["allow_unknown"] = draw(st.one_of(st.booleans(), rules_sets(2, validator)))
    if draw(one_in(5)):
        config["require_all"] = draw(st.booleans())
    if draw(one_in(5)):
        config["purge_unknown"] = draw(st.booleans())
    if draw(one_in(7)):
        config["purge_readonly"] = draw(st.booleans())
    if draw(one_in(7)):
        config["ignore_none_values"] = draw(st.booleans())
    if validator == "context" and draw(st.booleans()):
        config["limit"] = draw(small_ints)
    return config


@st.composite
def calls_for(draw, schema, max_calls=3):
    calls = []
    for _ in range(draw(st.integers(1, max_calls))):
        if draw(one_in(40)):
            document = draw(st.one_of(st.none(), st.just([]), st.just("doc"), st.just(1)))
        else:
            document = draw(document_for(schema))
            if draw(one_in(30)):
                document = odd_document(
                    document, draw(st.sampled_from(["ordered", "userdict", "proxy"]))
                )
        method = draw(
            st.sampled_from(["validate", "validate", "validate", "call", "normalized", "validated"])
        )
        kwargs = {}
        if method in ("validate", "call", "validated"):
            if draw(one_in(5)):
                kwargs["update"] = draw(st.booleans())
            if draw(one_in(4)):
                kwargs["normalize"] = draw(st.booleans())
        if method in ("normalized", "validated") and draw(one_in(3)):
            kwargs["always_return_document"] = draw(st.booleans())
        calls.append(Call(method, document, kwargs))
    return calls


@st.composite
def scenarios(draw, validators=("plain",), handlers=("basic",), junk=True):
    validator = draw(st.sampled_from(list(validators)))
    if junk and draw(one_in(25)):
        schema = draw(junk_schemas)
        calls = draw(calls_for(schema if isinstance(schema, dict) else {}, 1))
    else:
        schema = draw(mapping_schemas(0, validator))
        calls = draw(calls_for(schema))
    return Scenario(
        schema=schema,
        calls=calls,
        config=draw(configs(validator)),
        validator=validator,
        handler=draw(st.sampled_from(list(handlers))),
    )


@st.composite
def registry_scenarios(draw):
    """Schemas with registry references; the registries change between calls."""
    registry = draw(registries())
    schema = draw(st.one_of(mapping_schemas(0, "plain", True, True), SCHEMA_REFS))
    resolved = schema if isinstance(schema, dict) else registry["schemas"].get(schema, {})
    calls = []
    for _ in range(draw(st.integers(1, 4))):
        if draw(one_in(4)):
            kind = draw(st.sampled_from(["rules_sets", "schemas"]))
            if draw(one_in(4)):
                name = draw(RULES_REFS if kind == "rules_sets" else SCHEMA_REFS)
                calls.append(Call("registry_remove", (kind, name)))
            elif kind == "rules_sets":
                calls.append(
                    Call(
                        "registry_add",
                        (kind, draw(RULES_REFS), draw(rules_sets(1, "plain", True, True))),
                    )
                )
            else:
                calls.append(
                    Call(
                        "registry_add",
                        (kind, draw(SCHEMA_REFS), draw(mapping_schemas(1, "plain", True, True))),
                    )
                )
        calls.extend(draw(calls_for(resolved, 1)))
    config = draw(configs("plain"))
    if draw(one_in(5)):
        config["allow_unknown"] = draw(RULES_REFS)
    return Scenario(schema=schema, calls=calls, config=config, registry=registry)


@st.composite
def mutation_scenarios(draw, validators=("plain",)):
    """A validator whose schema is changed between calls."""
    validator = draw(st.sampled_from(list(validators)))
    schema = draw(mapping_schemas(0, validator))
    calls = list(draw(calls_for(schema, 2)))
    for _ in range(draw(st.integers(1, 3))):
        roll = draw(st.integers(0, 9))
        if roll == 0:
            calls.append(Call("clear_caches"))
        elif roll == 2:
            # a schema passed with the call replaces the validator's
            other = draw(mapping_schemas(0, validator))
            calls.append(Call("validate", draw(document_for(other)), {"schema": other}))
        elif roll == 1:
            calls.append(Call("set_schema", draw(mapping_schemas(0, validator))))
        else:
            method = draw(st.sampled_from(["mutate_raw", "mutate_raw", "mutate_schema"]))
            calls.append(Call(method, draw(schema_mutations(schema, validator))))
        calls.extend(draw(calls_for(schema, 2)))
    return Scenario(
        schema=schema, calls=calls, config=draw(configs(validator)), validator=validator
    )
