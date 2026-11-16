# Compatibility with Cerberus

Reference: Cerberus `1.3.x` at `83f5dada76aa3179ef21b677a7f4235bf20c1566`
(1.3.8). The goal is identical observable behavior; this file lists every
place where ferberus knowingly differs, and what the claim of compatibility
rests on.

## What is verified

- The **unmodified upstream test suite** (246 tests, 1 skipped upstream)
  passes against ferberus through the `cerberus` alias on CPython 3.9–3.15
  and free-threaded 3.14t/3.15t (`scripts/upstream-tests.sh`).
- **Differential property tests** (`tests/differential`) run random schemas,
  documents and call sequences through real Cerberus and ferberus and
  compare: return values, raised exception types and messages, the
  normalized document, the caller's input after the call, `_errors`
  (recursively: paths, codes, rules, constraints, values, info), `errors`,
  `recent_error`, both error trees, the state left on the validator
  (`update`, `_remaining_rules`, `_unrequired_by_excludes`,
  `_is_normalized`, the schema), the events an error handler sees, and
  warnings. Five properties: plain validators, error handlers, subclassed
  validators (custom rules, checks, coercers, setters, types, overridden
  built-ins, constructor context), registry references that change between
  calls, and schemas mutated between calls.
- `tests/regressions` keeps every discrepancy the fuzzer found.
- `tests/test_gaps.py` checks, against Cerberus, behaviors the upstream
  suite does not cover (extension points, document and value types, reuse,
  copy/pickle, schema changes, caches, registries).
- `tests/test_regex_fast.py` proves the `regex` fast path against `re`.

## Deviations

### Platform

1. **Interpreters.** CPython 3.9–3.15, free-threaded 3.14t and 3.15t.
   Cerberus declares `>=3.7` and PyPy; PyO3 0.30 needs CPython ≥ 3.9 and
   supports free-threading from 3.14. PyPy is not supported.
2. **No pure-Python wheel.** A compiler-less install is not possible; wheels
   are built per CPython version (no abi3, see DESIGN.md §4).

### Behavior that can differ

These are consequences of doing in native code what Cerberus does in
Python. None was observed with ordinary schemas and data.

3. **Calls into user objects.** Cerberus looks at documents, values and
   constraints many times per rule; ferberus looks as often as it needs.
   The number and order of calls to user-defined special methods
   (`__getitem__`, `__iter__`, `__len__`, `__eq__`, `__hash__`,
   `__contains__`, `__lt__`, `__instancecheck__`, `__copy__`, …) is therefore
   not preserved. Results are identical for objects whose special methods
   are deterministic and free of side effects. Exact `dict` documents are
   copied with `dict.copy()` instead of `copy.copy()` (the same thing).
4. **Built-in types and ABCs.** For values whose type is exactly `NoneType`,
   `bool`, `int`, `float`, `str`, `bytes`, `bytearray`, `list`, `tuple`,
   `dict`, `set` or `frozenset`, membership in `Mapping`, `Sequence`,
   `Iterable`, `Sized` and `Hashable` is taken from a table. Registering one
   of these built-ins with an ABC it does not belong to
   (`Mapping.register(list)`) has no effect. Subclasses and all other types
   go through `isinstance` as upstream.
5. **Child validators.** Cerberus creates a child `Validator` for every
   nested level. ferberus only creates one where Python code needs it: for
   levels whose rules call methods of the validator (custom rules,
   `check_with`, named coercers and setters, legacy type methods). A
   subclass' `__init__` consequently runs less often. What a hook sees as
   `self` is the same (class, `_config`, paths, `document`, `schema`,
   `root_*`); `tests/test_gaps.py::test_hooks_see_child_validators`.
6. **Recursion limits.** Processing deeply nested documents needs less of
   Python's stack. Where Cerberus raises `RecursionError` during validation
   (it does for an error about 30 levels deep, while it adds the error to
   its trees), ferberus may succeed and raise only when the error tree is
   requested, or not at all. With recursive registry definitions both raise
   `RecursionError`, not necessarily at the same depth.
7. **Documents changed while they are processed.** If Python code that is
   only reachable through special methods of values or constraints (not a
   hook, coercer or setter, whose effects are honored) adds or removes keys
   of the document under validation, Cerberus raises `RuntimeError:
   dictionary changed size during iteration`; ferberus works on the keys it
   saw first. The same holds for a coercer that changes the *outer* mapping
   of the container being normalized.
8. **Warnings.** The warning about colliding keys after `keysrules`
   normalization has the same category and text but is attributed to the
   caller's line instead of a line in `validator.py`.
9. **Tracebacks.** Exceptions have the same type and message; their
   tracebacks show `ferberus._core` instead of the frames of Cerberus'
   private methods.
10. **Monkeypatching the library itself.** Patching *validator classes*
    (also after creation, also instances) and the stock error handlers'
    methods is honored. Rebinding other module-level objects of ferberus —
    `errors.ValidationError`, `utils.compare_paths_lt`, the `re` module's
    `compile`, … — is not seen by the engine.
11. **Concurrent misuse.** Like upstream, a validator instance must not be
    shared between threads. What happens if it is, is undefined in both;
    ferberus guarantees that the interpreter survives, not the same wrong
    results.

### Internal representation

Not behavior of the documented API, but visible to code that looks:

12. `Validator.document_error_tree` / `schema_error_tree` are properties that
    build the tree on first access (from `_error_log`). `vars(validator)`
    has `_document_error_tree`, `_schema_error_tree` and `_error_log` instead
    of the two tree attributes. As long as error paths consist of `str` and
    `int`, the result is identical; with other key types the trees are
    maintained eagerly, as upstream, because adding to them can raise.
13. `DefinitionSchema.schema_validator` is created on first access (it is
    only needed for schemas not yet known as valid). Copies and pickles of
    a schema are what they are upstream.
14. Additional private attributes: `DefinitionSchema._ferberus_program`
    (compiled schema), `<validator class>._ferberus_class_info`,
    `InspectedValidator.__setattr__` / `__delattr__` (invalidate the class
    analysis).
15. The schema handling at the top of normalization (`schema.copy()` and
    the per-field re-validation, SURFACE.md quirk 6) runs once per compiled
    schema and again after `clear_caches()` or any change of the schema,
    instead of on every call. `Validator._valid_schemas` ends up with the
    same entries; emptying that set by hand (rather than through
    `clear_caches()`) does not trigger a re-run.
16. Compiled schemas are kept alive by the schema objects they belong to.

## Deliberately preserved quirks

See SURFACE.md ("Quirks that must be preserved") and the list of fuzzer
findings in NOTES.md. Things that look like bugs — the inconsistent ordering
of errors with mixed key types, `KeyError` when a coercer of an
`allow_unknown` rules set fails, `TypeError` swallowed by `min`/`max`,
deep-copied sets in error messages, renaming a field to itself deleting
it — behave as they do upstream.
