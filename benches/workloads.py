"""
Benchmark workloads shared by the Cerberus baseline and ferberus.

Every workload is a function ``(lib) -> callable``; ``lib`` is the module to
benchmark (real ``cerberus`` or ``ferberus``). The returned callable performs
one benchmark iteration and returns something cheap that the harness uses as
a sanity check (the same value must come out of both implementations).

Documents are generated with a fixed seed so runs are comparable.
"""

import random

WORKLOADS = {}


def workload(fn):
    WORKLOADS[fn.__name__] = fn
    return fn


# --------------------------------------------------------------------------
# small flat documents, one validator, many documents
# --------------------------------------------------------------------------

FLAT_SCHEMA = {
    "id": {"type": "integer", "min": 1, "required": True},
    "name": {"type": "string", "minlength": 1, "maxlength": 64, "required": True},
    "email": {"type": "string", "regex": r"[a-zA-Z0-9_.+-]+@[a-zA-Z0-9-]+\.[a-zA-Z0-9-.]+"},
    "age": {"type": "integer", "min": 0, "max": 150},
    "score": {"type": "float", "min": 0.0, "max": 100.0},
    "role": {"type": "string", "allowed": ["admin", "user", "guest"]},
    "active": {"type": "boolean"},
    "tags": {"type": "list", "maxlength": 8},
}


def flat_documents(n, invalid=False, seed=1):
    rnd = random.Random(seed)
    docs = []
    for i in range(n):
        doc = {
            "id": i + 1,
            "name": "user-%d" % i,
            "email": "user%d@example.com" % i,
            "age": rnd.randrange(0, 100),
            "score": rnd.random() * 100,
            "role": rnd.choice(["admin", "user", "guest"]),
            "active": bool(i & 1),
            "tags": ["a", "b", "c"][: rnd.randrange(0, 4)],
        }
        if invalid:
            doc["age"] = -1
            doc["role"] = "root"
            doc["email"] = "not an email"
            doc["extra"] = 1
        docs.append(doc)
    return docs


@workload
def small_flat_valid(lib):
    v = lib.Validator(FLAT_SCHEMA)
    docs = flat_documents(1000)

    def run():
        ok = 0
        for doc in docs:
            ok += v.validate(doc)
        return ok

    return run


@workload
def small_flat_invalid(lib):
    v = lib.Validator(FLAT_SCHEMA)
    docs = flat_documents(1000, invalid=True)

    def run():
        ok = 0
        for doc in docs:
            ok += v.validate(doc)
        return ok

    return run


@workload
def small_flat_invalid_errors(lib):
    """Like small_flat_invalid, additionally rendering ``Validator.errors``."""
    v = lib.Validator(FLAT_SCHEMA)
    docs = flat_documents(1000, invalid=True)

    def run():
        n = 0
        for doc in docs:
            v.validate(doc)
            n += len(v.errors)
        return n

    return run


@workload
def small_flat_no_normalize(lib):
    v = lib.Validator(FLAT_SCHEMA)
    docs = flat_documents(1000)

    def run():
        ok = 0
        for doc in docs:
            ok += v.validate(doc, normalize=False)
        return ok

    return run


# --------------------------------------------------------------------------
# deep nesting
# --------------------------------------------------------------------------


def nested_schema(depth):
    schema = {"leaf": {"type": "integer", "min": 0}, "label": {"type": "string"}}
    for _ in range(depth):
        schema = {
            "label": {"type": "string", "required": True},
            "child": {"type": "dict", "schema": schema, "required": True},
        }
    return schema


def nested_document(depth):
    doc = {"leaf": 1, "label": "leaf"}
    for i in range(depth):
        doc = {"label": "level-%d" % i, "child": doc}
    return doc


@workload
def deep_nesting(lib):
    v = lib.Validator(nested_schema(8))
    docs = [nested_document(8) for _ in range(200)]

    def run():
        ok = 0
        for doc in docs:
            ok += v.validate(doc)
        return ok

    return run


# --------------------------------------------------------------------------
# large lists
# --------------------------------------------------------------------------


@workload
def large_list_of_integers(lib):
    v = lib.Validator({"values": {"type": "list", "schema": {"type": "integer", "min": 0}}})
    doc = {"values": list(range(5000))}

    def run():
        return v.validate(doc)

    return run


ITEM_SCHEMA = {
    "sku": {"type": "string", "regex": r"[A-Z]{3}-\d{4}", "required": True},
    "qty": {"type": "integer", "min": 1, "max": 1000, "required": True},
    "price": {"type": "float", "min": 0},
    "tags": {"type": "list", "schema": {"type": "string", "maxlength": 12}},
}


def item_documents(n, seed=2):
    rnd = random.Random(seed)
    return [
        {
            "sku": "ABC-%04d" % (i % 10000),
            "qty": rnd.randrange(1, 1000),
            "price": rnd.random() * 50,
            "tags": ["t%d" % rnd.randrange(100) for _ in range(rnd.randrange(0, 4))],
        }
        for i in range(n)
    ]


@workload
def large_list_of_dicts(lib):
    v = lib.Validator(
        {"items": {"type": "list", "schema": {"type": "dict", "schema": ITEM_SCHEMA}}}
    )
    doc = {"items": item_documents(1000)}

    def run():
        return v.validate(doc)

    return run


@workload
def one_huge_document(lib):
    schema = {
        "meta": {
            "type": "dict",
            "schema": {"version": {"type": "integer"}, "source": {"type": "string"}},
        },
        "orders": {
            "type": "list",
            "schema": {
                "type": "dict",
                "schema": {
                    "id": {"type": "integer", "required": True},
                    "customer": {
                        "type": "dict",
                        "schema": {
                            "name": {"type": "string", "required": True},
                            "email": {"type": "string", "regex": r"[^@]+@[^@]+\.[^@]+"},
                        },
                    },
                    "items": {"type": "list", "schema": {"type": "dict", "schema": ITEM_SCHEMA}},
                    "attributes": {
                        "type": "dict",
                        "keysrules": {"type": "string", "regex": "[a-z_]+"},
                        "valuesrules": {"type": ["string", "integer"]},
                    },
                },
            },
        },
    }
    v = lib.Validator(schema)
    rnd = random.Random(3)
    items = item_documents(4000)
    doc = {
        "meta": {"version": 1, "source": "bench"},
        "orders": [
            {
                "id": i,
                "customer": {"name": "customer %d" % i, "email": "c%d@example.org" % i},
                "items": items[i * 2 : i * 2 + 4],
                "attributes": {"key_%s" % "abcdefgh"[j]: rnd.choice(["x", 1]) for j in range(4)},
            }
            for i in range(2000)
        ],
    }

    def run():
        return v.validate(doc)

    return run


# --------------------------------------------------------------------------
# *of rules
# --------------------------------------------------------------------------


@workload
def anyof_oneof(lib):
    schema = {
        "value": {
            "anyof": [
                {"type": "integer", "min": 0, "max": 10},
                {"type": "string", "regex": "[a-z]+"},
                {"type": "list", "schema": {"type": "integer"}},
            ]
        },
        "ident": {"oneof": [{"type": "integer", "min": 1}, {"type": "string", "minlength": 8}]},
        "kind": {"anyof_type": ["string", "integer"]},
        "limit": {"type": "integer", "noneof": [{"min": 100}, {"max": -1}]},
    }
    v = lib.Validator(schema)
    rnd = random.Random(4)
    docs = [
        {
            "value": rnd.choice([5, "abc", [1, 2, 3]]),
            "ident": rnd.choice([42, "abcdefghij"]),
            "kind": rnd.choice(["a", 1]),
            "limit": rnd.randrange(0, 100),
        }
        for _ in range(500)
    ]

    def run():
        ok = 0
        for doc in docs:
            ok += v.validate(doc)
        return ok

    return run


# --------------------------------------------------------------------------
# normalization
# --------------------------------------------------------------------------


@workload
def normalization_heavy(lib):
    schema = {
        "id": {"coerce": int, "type": "integer"},
        "amount": {"coerce": float, "type": "float"},
        "label": {"coerce": (str, str.strip), "type": "string"},
        "status": {"type": "string", "default": "new"},
        "retries": {"type": "integer", "default": 0},
        "legacy_name": {"rename": "name"},
        "name": {"type": "string"},
        "options": {
            "type": "dict",
            "purge_unknown": True,
            "schema": {
                "verbose": {"type": "boolean", "default": False},
                "level": {"coerce": int, "type": "integer", "default": 1},
            },
        },
        "values": {"type": "list", "schema": {"coerce": int, "type": "integer"}},
    }
    v = lib.Validator(schema, purge_unknown=True)
    docs = [
        {
            "id": str(i),
            "amount": "%d.5" % i,
            "label": "  label %d " % i,
            "legacy_name": "n%d" % i,
            "junk": 1,
            "more_junk": [1, 2],
            "options": {"level": "3", "debug": True},
            "values": ["1", "2", "3", 4],
        }
        for i in range(500)
    ]

    def run():
        ok = 0
        for doc in docs:
            ok += v.validated(doc) is not None
        return ok

    return run


# --------------------------------------------------------------------------
# regex
# --------------------------------------------------------------------------


@workload
def regex_heavy(lib):
    schema = {
        "uuid": {
            "type": "string",
            "regex": r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}",
        },
        "date": {"type": "string", "regex": r"\d{4}-\d{2}-\d{2}"},
        "slug": {"type": "string", "regex": r"^[a-z0-9]+(?:-[a-z0-9]+)*$"},
        "phone": {"type": "string", "regex": r"\+?\d{1,3}[ -]?\(?\d{3}\)?[ -]?\d{3}[ -]?\d{4}"},
        "word": {"type": "string", "regex": r"(?i)(alpha|beta|gamma)\w*"},
        "doubled": {"type": "string", "regex": r"(\w+) \1"},
    }
    v = lib.Validator(schema)
    docs = [
        {
            "uuid": "0123abcd-0123-4567-89ab-%012x" % i,
            "date": "2026-11-%02d" % (i % 28 + 1),
            "slug": "some-slug-%d" % i,
            "phone": "+1 (555) 010-%04d" % (i % 10000),
            "word": "Gamma%d" % i,
            "doubled": "hey hey",
        }
        for i in range(500)
    ]

    def run():
        ok = 0
        for doc in docs:
            ok += v.validate(doc)
        return ok

    return run


# --------------------------------------------------------------------------
# construction / schema validation
# --------------------------------------------------------------------------


def big_schema(nfields=60):
    schema = {}
    for i in range(nfields):
        kind = i % 6
        if kind == 0:
            rules = {"type": "integer", "min": i, "max": i + 1000, "required": i % 4 == 0}
        elif kind == 1:
            rules = {
                "type": "string",
                "minlength": 1,
                "maxlength": 10 + i,
                "regex": "[a-z]{%d,}" % (i % 5),
            }
        elif kind == 2:
            rules = {"type": "list", "schema": {"type": "integer", "allowed": list(range(i + 2))}}
        elif kind == 3:
            rules = {
                "type": "dict",
                "schema": {"a%d" % i: {"type": "string"}, "b": {"type": "float", "nullable": True}},
            }
        elif kind == 4:
            rules = {"anyof": [{"type": "integer", "min": i}, {"type": "string", "empty": False}]}
        else:
            rules = {"type": "boolean", "default": bool(i & 2), "dependencies": ["field_0"]}
        schema["field_%d" % i] = rules
    return schema


@workload
def schema_validation_cold(lib):
    """Validator construction including full schema validation (caches cleared)."""
    schema = big_schema()

    def run():
        lib.Validator.clear_caches()
        return len(lib.Validator(schema).schema)

    return run


@workload
def validator_construction_warm(lib):
    """Validator construction with the schema already known as valid."""
    schema = big_schema()
    lib.Validator(schema)

    def run():
        n = 0
        for _ in range(20):
            n += len(lib.Validator(schema).schema)
        return n

    return run


@workload
def validator_construction_small(lib):
    lib.Validator(FLAT_SCHEMA)

    def run():
        n = 0
        for _ in range(100):
            n += len(lib.Validator(FLAT_SCHEMA).schema)
        return n

    return run


# --------------------------------------------------------------------------
# extension points
# --------------------------------------------------------------------------


@workload
def custom_rules(lib):
    class MyValidator(lib.Validator):
        def _validate_is_even(self, constraint, field, value):
            """{'type': 'boolean'}"""
            if constraint and value % 2:
                self._error(field, "must be even")

        def _check_with_short(self, field, value):
            if len(value) > 20:
                self._error(field, "too long")

        def _normalize_coerce_upper(self, value):
            return value.upper()

    def positive(field, value, error):
        if value <= 0:
            error(field, "must be positive")

    schema = {
        "n": {"type": "integer", "is_even": True, "check_with": positive},
        "s": {"type": "string", "check_with": "short", "coerce": "upper"},
        "plain": {"type": "integer", "min": 0},
        "name": {"type": "string", "maxlength": 30},
        "nested": {"type": "dict", "schema": {"m": {"type": "integer", "is_even": True}}},
    }
    v = MyValidator(schema)
    docs = [
        {"n": 2 * i + 2, "s": "abc%d" % i, "plain": i, "name": "n%d" % i, "nested": {"m": 2 * i}}
        for i in range(500)
    ]

    def run():
        ok = 0
        for doc in docs:
            ok += v.validate(doc)
        return ok

    return run
