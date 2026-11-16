# ferberus design

Target: behave exactly like Cerberus 1.3.x (`83f5dad`, see `SURFACE.md`) and
be much faster. Rule of the house: **correctness first; a fast path that is
not provably identical is gated behind a fallback.**

## 1. Shape of the solution

```
            python/ferberus/*.py                     src/*.rs  (ferberus._core)
  ┌────────────────────────────────────┐   ┌──────────────────────────────────────┐
  │ Validator / BareValidator          │   │ class analysis   (which hooks are   │
  │   object model, metaclass,         │   │                   overridden)        │
  │   docstring rule schemas,          │   │ schema compiler  (dict -> program,   │
  │   properties, _error, ...          │──▶│                   + mutation guard)  │
  │ DefinitionSchema, registries       │   │ engine           (normalize/validate │
  │ errors: ValidationError, trees,    │◀──│                   frames)            │
  │   handlers                         │   │ error building, sorting, rendering   │
  │ reference engine (ported Cerberus) │   │ regex fast path                      │
  └────────────────────────────────────┘   └──────────────────────────────────────┘
```

- The **Python layer is a port of Cerberus' own modules** (ISC). It defines
  every class a user can see, subclass, pickle or monkeypatch, so the object
  model is identical by construction: `Validator.__mro__`, the
  `InspectedValidator` metaclass, docstring-embedded rule schemas,
  `validator_factory`, `DefinitionSchema`, registries, error classes.
  It also contains the complete **reference engine** (Cerberus' algorithms,
  unchanged).
- The **Rust core** is an accelerator for `validate()` / `normalized()` /
  `validated()` / `errors`. It operates on the very same Python objects and
  leaves the validator in the state the reference engine would have left.
- Per call, the core either runs natively or **declines**, in which case the
  reference engine runs. Declining is always safe, so the native engine only
  has to implement what it can reproduce exactly.

This split also gives the upstream suite a meaningful role from day one: it
passes against the pure port (246/246), and every step of moving work into
Rust must keep it there, while the differential fuzzer compares the native
engine against real Cerberus.

## 2. Hybrid execution

### 2.1 Class analysis

At class creation (metaclass) and lazily on first use, a validator class is
analysed once and the result cached, keyed by a global *class epoch* that is
bumped by `InspectedValidator.__setattr__/__delattr__` (monkeypatching a
validator class invalidates every cached analysis).

- **Engine seams** — `_error`, `_get_child_validator`, `_lookup_field`,
  `_drop_remaining_rules`, `_drop_nodes_from_errorpaths`, `_resolve_schema`,
  `_resolve_rules_set`, the name-mangled privates, the config properties,
  `mandatory_validations`, `priority_validations`: if any differs from
  `BareValidator`'s original, the class is *exotic* and always uses the
  reference engine.
- **Rule handlers** — for every `_validate_<rule>` / `_normalize_<rule>`:
  the original function object ⇒ native implementation; anything else ⇒ a
  Python call *for that rule only*.
- Instances are checked per call for attributes shadowing a handler or seam
  (`v._validate_min = ...`).

### 2.2 Frames

Cerberus processes every nesting level with a child `Validator` instance.
The engine mirrors that with **frames**: one frame per (child) validator
invocation, carrying the document, the compiled schema level, the effective
configuration (`allow_unknown`, `require_all`, `purge_unknown`, …), document
and schema path, `update`, `_is_normalized`, and the error list. Each child
is run in one of three modes, decided per child from what was compiled for
it and from the document at hand:

| Mode | Validator object | Body | Used for |
| --- | --- | --- | --- |
| **native** | none | Rust | schemas of built-in rules only |
| **object** | a real child validator | Rust | levels whose rules call methods of the validator |
| **Python** | a real child validator | reference implementation | anything the compiler marked unsupported, odd document types, very deep nesting |

- A **native frame** has no Python validator object. A schema made only of
  built-in rules runs through native frames exclusively: the interpreter is
  touched only to read the input (type checks, comparisons, hashing — C-API
  calls on the caller's objects) and to create result objects.
- **Callables without `self`** (`coerce`, `default_setter`, `rename_handler`
  given as callables) are called from native frames, exactly where they
  appear.
- **Hooks needing `self`** — `_validate_<custom>`, overridden built-in
  rules, `_check_with_<name>`, `check_with` callables (they receive the
  bound `self._error`), `_normalize_coerce_<name>`,
  `_normalize_default_setter_<name>`, `_validate_type_<name>` — run in
  **object frames**: the root validator, or a child validator that is what
  `_get_child_validator` would have produced (class, full `_config`,
  `root_*`, paths, document, schema). If the class has the stock `__new__`
  and `__init__`, the child is assembled natively (its `__dict__` is filled
  in directly); otherwise Python constructs it. While such a frame runs,
  the Python-visible state (`document`, `_errors`, `recent_error`,
  `_remaining_rules`, `_unrequired_by_excludes`, `_is_normalized`, the
  trees if they exist) is live, because hooks read and change it:
  the rules queue of a field with hooks *is* `self._remaining_rules`.
- Children of object and native frames are again decided individually, so
  only the levels that contain hooks pay for validator objects.
- The engine is entered for **any** validator that is not exotic, including
  child validators that Python code calls (the reference implementation
  running a delegated level, a hook that uses `_get_child_validator`, the
  schema validator's checks). Their schemas are plain dicts; programs for
  those are cached by the dict's address (the program keeps the dict alive).

### 2.3 Errors

`ValidationError` objects are real Python objects, created natively and
eagerly (they are the documented result type). Per frame they live in an
`ErrorList`; like Cerberus, the list is sorted after each submission.

- Sorting uses a native comparator (binary insertion) for the *clean* case:
  paths made of exact `str`/`int` elements, no two errors with equal paths
  and no `int` meeting a `str` at the same position — there the order is
  unique. As soon as a comparison is not clear-cut, the list is sorted by
  Python's own `list.sort()` with `ValidationError.__lt__`, so the
  inconsistent comparator (SURFACE quirk 2) behaves exactly as upstream on
  the running interpreter.
- `document_error_tree` / `schema_error_tree` are built lazily from the
  submission log when first accessed (properties on `BareValidator`). The
  engine answers its own tree queries (`readonly`, coercion chains,
  `dependencies`) from the log. Laziness ends where it could be observed:
  once an error path holds anything but `str`/`int` (adding such an error
  to a tree can raise, which upstream does *during* validation), or once
  Python code has built the trees, they are maintained eagerly.
- `Validator.errors` with an unmodified `BasicErrorHandler` /
  `SchemaErrorHandler` is rendered natively. The Python handlers deep-copy
  every error first; the native rendering computes the rewritten paths on
  the side instead, and is only used for errors holding plain built-in
  values, for which the copy cannot fail or be observed (sets are rebuilt
  like `deepcopy` does, since that can change their printed order). Other
  handlers are called as usual, including `start` / `emit` / `end`.

## 3. Schema compilation

### 3.1 When and what

`DefinitionSchema` (Python) keeps doing what Cerberus does on assignment:
expansion in the caller's dict, validation, `_valid_schemas` caching. The
compiled program is created lazily at the first native run and cached on the
`DefinitionSchema` object, keyed by the validator class and class epoch.

Programs for schemas that are plain dicts (child validators, see 2.2) are
cached globally by the dict's address, bounded to 256 entries and emptied by
`clear_caches()`.

A program is a tree of **levels** (field → rules set) and **rule sets**. A
rule set holds: the normalization rules, pre-resolved; the validation queue
in final order (`nullable`, `readonly`, `type`, `empty`, then schema order)
with each entry resolved to a native op or a Python handler; and
precomputed facts (does this level rename / default / coerce at all, which
fields are required, can normalization of this subtree change anything, does
it need `self`). Type names are resolved against `types_mapping` into a
bitmask over the common exact types plus the `isinstance` fallback tuple.
Validation never re-interprets schema dicts.

### 3.2 Declining

The compiler is total: for anything outside the well-formed mainstream it
marks the smallest enclosing unit (rules set or level) as *unsupported*
rather than guessing — non-`dict` mappings as schemas, rule names with
spaces, rules without a handler, constraints of unexpected types, a mapping
schema that would be applied to a sequence (and vice versa), references
that do not resolve or are recursive, and the few places where Cerberus
reads an unresolved reference (`excludes` and `*of` behind a rules-set
reference). An unsupported child is run by the reference implementation
(mode "Python" above); an unsupported root declines the call.

**Registry references** are resolved at compile time; the program then
depends on the registry entries it used (see the guard).

### 3.3 Mutation guard

Cerberus reads the user's schema dicts live; in-place mutation after
construction is visible to the next validation (and, with normalization,
re-validated through the per-call `schema.copy()`). To keep that behavior, a
compiled program carries a **guard**: a snapshot of the identity of every
key and value in every container `mapping_hash` would traverse, plus all
of `types_mapping` (it is part of the key of `_valid_schemas`) and the
registry entries the program resolved. Lists are compared by their members,
because expanding a schema replaces the lists of `*of`/`items` rules with
equal ones on every pass of the reference implementation. The guard is re-checked at the start of
each call (pointer comparisons, no allocation); on mismatch the program is
recompiled and the reference `schema.copy()` dance (SURFACE quirk 6) is
re-run once so that re-validation errors and `_valid_schemas` contents match
upstream. Constraint *values* (`allowed` containers, `min`, …) are never
copied into the program; they are evaluated live.

## 4. Crossing cost: read Python objects in place

Measured (`benches/exp_crossing.py`, CPython 3.14.10, i7-8700K): identical
work done on the Python objects directly vs. on a converted Rust tree
(`enum Value`):

| document | direct | convert only | convert + work | ratio |
| --- | ---: | ---: | ---: | ---: |
| flat, 8 fields | 0.17 µs | 0.79 µs | 0.87 µs | 5.1× |
| nested, 8 levels | 0.32 µs | 1.45 µs | 1.61 µs | 5.0× |
| 1000 item dicts | 77 µs | 722 µs | 830 µs | 10.8× |
| 5000 ints | 31 µs | 70 µs | 81 µs | 2.6× |

Conversion alone costs several times the whole traversal, and it cannot
represent what Cerberus semantics need anyway (object identity, `bool` vs
`int`, subclasses, arbitrary user types, in-place normalization of the
caller's containers). **Decision: no conversion.** The engine reads the
caller's objects through the C-API with fast paths for exact built-in types
and the generic protocol (`isinstance` against the ABCs, rich comparison,
`in`, `len`) for everything else. Copies are made exactly where Cerberus
makes them (shallow top-level copy; rebuilt containers during
normalization).

Consequence: built for a specific CPython ABI (not abi3), because the hot
path uses `PyDict_Next`, `PyList_GET_ITEM`, type flags and compact-string
access.

## 5. Regex

Cerberus: `re.compile(pattern if pattern.endswith('$') else pattern + '$').match(value)`.

1. **Reference path:** the compiled Python `re.Pattern` is cached in the
   program and called through the C-API. Exact by definition; used whenever
   the fast path does not apply. Invalid patterns raise the same `re.error`
   at the same time (validation, not schema compilation).
2. **Fast path (`regex` crate, bytes API, Unicode off):** only if the
   pattern and the subject are ASCII *and* a purpose-built translator
   (`crates/ferberus-regex`) accepts the pattern. The translator parses
   Python's syntax itself and emits an explicit pattern: every literal as a
   hex escape and every class — including `\d`, `\w`, `\s` (with
   `\x1c`–`\x1f`), `.`, negation and `(?i)` case folding — as a list of
   byte ranges it computed, so no difference in class or escape syntax
   between the engines can change the meaning. `$` is accepted only as the
   last element of a top-level alternative and becomes `\n?\z`; `^`/`\A`
   become `\A`, `\Z` becomes `\z`. It rejects everything whose semantics
   differ or are unsupported: back-references, look-around, conditionals,
   atomic groups and possessive quantifiers, comments, flags other than
   leading `i`/`s`/`u`, octal and Unicode escapes, `{` that is not a
   quantifier, constructs Python warns about (nested sets, `--`, `&&`, …),
   and repetition of groups that can match the empty string (where `sre`'s
   loop guards are not obviously regular). Since the question is boolean
   and anchored at 0, leftmost-first vs. backtracking priority cannot matter
   for this subset.
3. `fancy-regex` is **not** used: its backtracking semantics for
   back-references and look-around cannot be proven identical to `sre`,
   and the Python fallback already covers those patterns exactly.

Proof: `tests/test_regex_fast.py` generates patterns from the accepted
grammar, from near misses and from arbitrary text, with arbitrary ASCII
subjects, and asserts that for every accepted pattern `re.compile` neither
fails nor warns and `match()` agrees. `fuzz/` (cargo-fuzz) checks that the
translator never panics and only emits valid patterns. About a third of the
generated patterns are accepted; the typical validation patterns (e-mail,
UUID, dates, slugs, identifiers) are.

## 6. Python API layer

- `import ferberus` — primary name; same module layout as `cerberus`.
- `ferberus.compat.install()` — meta-path alias so that `import cerberus`
  (and `cerberus.errors`, `.schema`, `.utils`, `.validator`, `.platform`)
  resolve to the ferberus modules. `-p ferberus.compat` does it for pytest;
  with `FERBERUS_UPSTREAM_PACKAGE` the upstream `tests`/`benchmarks`
  packages are importable below `cerberus`, which is how the unmodified
  suite runs.

## 7. Concurrency

- A `Validator` instance is not thread-safe in Cerberus (per-run state on
  the instance) and is not in ferberus; concurrent use of *separate*
  instances is supported, including instances sharing a schema object.
- Shared native state: compiled programs (immutable after construction,
  replaced atomically), class-analysis cache and counters (atomics /
  `Mutex`). The extension declares `gil_used = false`.
- Free-threaded builds: the engine never holds borrowed references across
  calls that can run arbitrary code, and iterates dicts under their critical
  section. Misuse (mutating a document from another thread during
  validation) must not crash; results are then unspecified, as upstream.
- **GIL release:** the engine works on Python objects almost all the time,
  so there is little to release. The exception is the regex fast path: the
  subject's UTF-8 buffer is immutable and kept alive, so matching needs no
  interpreter. Subjects of at least 4096 bytes are matched detached; the
  threshold comes from `benches/exp_gil_release.py` (BENCHMARKS.md).

## 8. Verification strategy

1. Upstream suite, unmodified, against the alias (`scripts/upstream-tests.sh`).
2. Differential property tests (Hypothesis): random schemas × documents,
   real Cerberus vs ferberus — result, `_errors` (paths, codes, rules,
   constraints, values, info, recursively), `errors`, both trees, normalized
   document, input mutation, exceptions (type and message), warnings.
   Discrepancies become permanent regression tests.
3. The same harness runs ferberus with the native engine disabled, which
   pins the port itself.
4. Engine statistics (`ferberus._core.stats()`): how many frames ran
   natively, were delegated or declined, and why — reported per milestone so
   "passes" cannot hide "everything fell back".
5. Rust unit tests (nextest) and cargo-fuzz for the pure-Rust parts (regex
   translator, path comparison).
