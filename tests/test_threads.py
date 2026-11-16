"""
Concurrency: correct with the GIL and on free-threaded builds.

A ``Validator`` is not thread-safe (neither is Cerberus': the state of a
run lives on the instance), so the supported pattern is one validator per
thread. Validators may share schemas, registries and classes. Sharing a
validator between threads must not crash the interpreter.
"""

import sys
import threading

import ferberus
import pytest
from ferberus import _core

SCHEMA = {
    "id": {"type": "integer", "min": 0, "required": True},
    "name": {"type": "string", "regex": "[a-z]+", "coerce": str.strip},
    "tags": {"type": "list", "schema": {"type": "string", "maxlength": 4}},
    "child": {
        "type": "dict",
        "schema": {
            "x": {"type": "integer", "default": 1},
            "y": {"anyof_type": ["string", "integer"]},
        },
    },
    "extra": {"type": "dict", "keysrules": {"type": "string"}, "valuesrules": {"type": "integer"}},
}


def good(i):
    return {"id": i, "name": " abc ", "tags": ["a", "b"], "child": {"y": i}, "extra": {"k": i}}


def bad(i):
    return {
        "id": -i - 1,
        "name": "ABC",
        "tags": ["toolong", 1],
        "child": {"x": "no", "y": 1.5},
        "extra": {1: "x"},
        "unknown": 1,
    }


def run_threads(target, count=8):
    failures = []
    barrier = threading.Barrier(count)

    def wrapper(index):
        try:
            barrier.wait()
            target(index)
        except BaseException as exc:  # noqa: BLE001
            failures.append(exc)

    threads = [threading.Thread(target=wrapper, args=(i,)) for i in range(count)]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()
    if failures:
        raise failures[0]


def test_extension_does_not_enable_the_gil():
    if hasattr(sys, "_is_gil_enabled") and "t" in sys.abiflags:
        assert not sys._is_gil_enabled()


def test_one_validator_per_thread_sharing_the_schema_dict():
    reference = ferberus.Validator(SCHEMA)
    assert not reference.validate(bad(0))
    expected_errors = reference.errors
    assert reference.validate(good(0))
    expected_document = reference.document

    def work(index):
        validator = ferberus.Validator(SCHEMA)
        for i in range(300):
            assert validator.validate(good(0)), validator.errors
            assert validator.document == expected_document
            assert not validator.validate(bad(0))
            assert validator.errors == expected_errors

    run_threads(work)


def test_validators_sharing_one_definition_schema_and_program():
    first = ferberus.Validator(SCHEMA)
    validators = [ferberus.Validator(first.schema) for _ in range(8)]
    expected = []
    for i in range(50):
        first.validate(bad(i) if i % 2 else good(i))
        expected.append((bool(first._errors), first.errors, first.document))

    def work(index):
        validator = validators[index]
        for _ in range(20):
            for i in range(50):
                validator.validate(bad(i) if i % 2 else good(i))
                assert (bool(validator._errors), validator.errors, validator.document) == expected[
                    i
                ]

    run_threads(work)


def test_subclass_with_hooks_and_registries_in_threads():
    class Custom(ferberus.Validator):
        def _check_with_even(self, field, value):
            if value % 2:
                self._error(field, "odd")

    ferberus.rules_set_registry.add("threaded_even", {"type": "integer", "check_with": "even"})
    try:
        schema = {
            "n": "threaded_even",
            "items": {"type": "list", "schema": {"type": "dict", "schema": {"m": "threaded_even"}}},
        }

        def work(index):
            validator = Custom(schema)
            for i in range(200):
                ok = validator.validate({"n": i, "items": [{"m": i}, {"m": 2}]})
                assert ok is (i % 2 == 0)
                if not ok:
                    assert validator.errors == {"n": ["odd"], "items": [{0: [{"m": ["odd"]}]}]}

        run_threads(work)
    finally:
        ferberus.rules_set_registry.remove("threaded_even")


def test_schema_mutation_while_other_threads_validate():
    """Recompilation after a guard miss while other threads use the old program."""
    schema = {"a": {"type": "integer", "min": 0}, "b": {"type": "string"}}
    validators = [ferberus.Validator(ferberus.Validator(schema).schema) for _ in range(8)]
    stop = threading.Event()

    def mutate():
        value = 0
        while not stop.is_set():
            value += 1
            schema["a"]["min"] = -value

    mutator = threading.Thread(target=mutate)
    mutator.start()
    try:

        def work(index):
            validator = validators[index]
            for _ in range(2000):
                # valid for every value `min` can take
                assert validator.validate({"a": 5, "b": "x"}), validator.errors

        run_threads(work)
    finally:
        stop.set()
        mutator.join()


def test_sharing_a_validator_between_threads_does_not_crash():
    """Results are unspecified (as upstream), the interpreter must survive."""
    validator = ferberus.Validator(SCHEMA)

    def work(index):
        for i in range(300):
            try:
                validator.validate(bad(i) if (i + index) % 2 else good(i))
                validator.errors
                validator.document_error_tree
            except Exception:  # noqa: BLE001 - anything but a crash is fine
                pass

    run_threads(work)
    # and it still works afterwards
    assert validator.validate(good(1))
    assert not validator.validate(bad(1))


def test_documents_mutated_by_another_thread_do_not_crash():
    validator = ferberus.Validator(SCHEMA, allow_unknown=True)
    document = good(1)
    stop = threading.Event()

    def mutate():
        i = 0
        while not stop.is_set():
            i += 1
            document["tags"].append("x")
            document["extra"]["k%d" % (i % 50)] = i
            if len(document["tags"]) > 30:
                del document["tags"][:]
            document["extra"].pop("k%d" % ((i * 7) % 50), None)

    mutator = threading.Thread(target=mutate)
    mutator.start()
    try:
        for _ in range(3000):
            try:
                validator.validate(document)
            except Exception:  # noqa: BLE001
                pass
    finally:
        stop.set()
        mutator.join()


@pytest.mark.parametrize("size", [100, 20000])
def test_regex_fast_path_from_threads(size):
    """Long subjects are matched with the interpreter released."""
    validator_schema = {"s": {"type": "string", "regex": "(?:[a-z]+ )*[a-z]+"}}
    subject = " ".join(["word"] * (size // 5))
    assert _core.regex_fast_translate("(?:[a-z]+ )*[a-z]+$") is not None

    def work(index):
        validator = ferberus.Validator(validator_schema)
        for _ in range(100):
            assert validator.validate({"s": subject})
            assert not validator.validate({"s": subject + " 1"})

    run_threads(work)
