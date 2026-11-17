# Benchmarks

All numbers were measured inside the dev container on one machine:

| | |
| --- | --- |
| CPU | Intel Core i7-8700K, 6 cores / 12 threads, 3.7 GHz |
| Memory | 64 GB |
| Host | Linux 7.0.0-43, Docker 29.9.3 |
| Image | `sha256:7fa2be0f488c8147c76cfab653ba6da58d832a088e0a6019ab84eb90f8398da5` |
| Reference | Cerberus `83f5dad` (1.3.8), pure Python |
| ferberus | commit `8792d8c`, release profile (`lto = "fat"`, `codegen-units = 1`) |

Reproduce: `./dev python benches/run.py --json out.json` and
`./dev python benches/report.py out.json`. Raw results are in
`benches/results/`.

Method: every workload (`benches/workloads.py`) is a function that is run
against both libraries in the same process. It is calibrated to at least
0.2 s per round; the best of 7 rounds is reported, with the garbage
collector disabled during the measured rounds. The harness also checks that
both libraries return the same result.

## 1. Where Cerberus spends its time (Phase 0)

Baseline of the bundled benchmarks: `test_overall_performance_1` 8.16 s,
`test_overall_performance_2` 8.48 s (min of 5 rounds).

Profiles (`benches/prof.py`, cProfile and py-spy; flame graphs in
`profiles/`):

- **Normalization re-validates the schema on every call.** With the default
  `normalize=True`, `__normalize_mapping` starts with `schema.copy()` and an
  item assignment per field on a `DefinitionSchema`; each of them hashes the
  schema (`mapping_hash` → `mapping_to_frozenset`). For small flat documents
  that is ~65 % of the time: 301 µs per document with normalization, 36 µs
  without. This is the largest single cost and the reason why the speedup
  with normalization is so much higher than without.
- **Rule dispatch.** Every rule of every field goes through
  `__get_rule_handler` (string formatting + `getattr`) and a closure; type
  checks are `isinstance` calls against ABCs (`__instancecheck__` in
  Python).
- **Child validators.** Every nested mapping, every sequence and every
  `*of` definition instantiates a `Validator` (`__init__`,
  `_get_child_validator`, `__init_processing`) and builds a throw-away
  schema and document (`{i: rules}`, `{i: value}`). In the list-of-dicts
  workload that is ~45 % of the time.
- **Errors.** Each error is sorted into `_errors`, added to two trees, and
  later deep-copied by the error handler.

That set the order of work: compile schemas once and guard them (instead of
re-validating), dispatch on precompiled operations, avoid child validators
and their dicts, and build error structures lazily.

## 2. Results

CPython 3.14.10:

| Workload | Cerberus | ferberus | Speedup |
| --- | ---: | ---: | ---: |
| 1000 small flat documents, valid | 301.26 ms | 1.82 ms | **165.3×** |
| 1000 small flat documents, 4 errors each | 353.25 ms | 3.86 ms | **91.6×** |
| … plus rendering `Validator.errors` | 424.65 ms | 8.22 ms | **51.7×** |
| 1000 small flat documents, `normalize=False` | 36.14 ms | 1.85 ms | **19.5×** |
| 200 documents nested 8 levels deep | 97.28 ms | 1.05 ms | **92.9×** |
| one list of 5000 integers | 24.83 ms | 267.9 µs | **92.7×** |
| one list of 1000 dicts | 73.65 ms | 1.05 ms | **70.1×** |
| one document: 2000 orders × items × attributes | 928.31 ms | 12.93 ms | **71.8×** |
| 500 documents against `anyof`/`oneof`/`noneof` | 255.96 ms | 4.33 ms | **59.2×** |
| 500 documents: coerce, default, rename, purge | 227.77 ms | 2.66 ms | **85.7×** |
| 500 documents, 6 `regex` rules each | 136.35 ms | 1.04 ms | **131.7×** |
| `Validator(schema)`, 60 fields, caches cleared | 10.45 ms | 4.53 ms | **2.3×** |
| 20 × `Validator(schema)`, 60 fields, schema known | 11.23 ms | 4.35 ms | **2.6×** |
| 100 × `Validator(schema)`, 8 fields, schema known | 6.58 ms | 2.28 ms | **2.9×** |
| 500 documents, subclass with custom rule, checks, coercer | 124.21 ms | 3.36 ms | **37.0×** |

Per document that is 1.8 µs for a valid 8-field document (Cerberus:
301 µs), 3.9 µs for one with four errors.

Cerberus' own benchmarks, unmodified, through the alias
(`scripts/upstream-benchmarks.sh`, min of 5 rounds):

| Benchmark | Cerberus | ferberus | Speedup |
| --- | ---: | ---: | ---: |
| `test_overall_performance_1` (10 000 documents; registries, `check_with`, `*of`, dependencies, normalization; 62 % invalid) | 8.16 s | 0.309 s | **26×** |
| `test_overall_performance_2` (10 885 product documents, `purge_unknown`, defaults, coercion) | 8.48 s | 0.096 s | **88×** |

Other interpreters (speedup; full tables via `benches/report.py`):

| Workload | 3.12 | 3.14 | 3.14t |
| --- | ---: | ---: | ---: |
| small flat, valid | 180× | 165× | 177× |
| small flat, invalid | 99× | 92× | 92× |
| invalid + `errors` | 53× | 52× | 49× |
| `normalize=False` | 21× | 20× | 18× |
| deep nesting | 96× | 93× | 85× |
| list of integers | 95× | 93× | 100× |
| list of dicts | 63× | 70× | 68× |
| one huge document | 71× | 72× | 71× |
| `*of` | 66× | 59× | 57× |
| normalization | 89× | 86× | 84× |
| regex | 143× | 132× | 152× |
| schema validation, cold | 2.3× | 2.3× | 2.2× |
| construction, warm | 2.5× | 2.6× | 3.1× |
| custom rules | 39× | 37× | 35× |

### What is not fast

- **Validator construction and schema validation (2–3×).** Construction
  still runs Cerberus' Python code for expanding the schema and checking the
  cache; the gain comes from a native `mapping_hash` and from not creating
  the schema validator unless it is needed. Validating a schema is done by
  the schema validator's Python hooks, each of which the engine calls back.
- **Custom Python rules** cost what calling Python costs; the 37× comes from
  everything around them.
- **Classes that override engine internals** (`_error`,
  `_get_child_validator`, …) run on the reference implementation: 1×.

`ferberus._core.stats()` tells which path was taken. For the upstream test
suite (`scripts/upstream-stats.py`): 924 calls were processed by the engine
(1100 frames without validator object, 716 with one), 13 calls were declined
because of the schema (11 of them schemas whose field definitions are not
rules sets — the tests of broken schemas) and 9 child validators were handed
to the reference implementation. No call was declined because of the
validator class.

## 3. ferberus profile

`perf` on the small-flat workload (1.8 µs per call):

| Share | Where |
| ---: | --- |
| ~32 % | rule evaluation for 8 fields / ~20 rules (`validate_frame`, of which the `regex` match was 9.5 points before the fast path) |
| ~20 % | the Python-level `validate()` wrapper and the benchmark loop |
| ~12 % | mutation guard: comparing ~60 pointers of the schema dicts |
| ~10 % | resetting the validator: `ErrorList()`, `set()`, list, eight attribute writes, `dict.copy()` |
| ~9 % | reading the validator's configuration, class analysis lookup |
| rest | allocation, reference counting, dict lookups |

There is no dominant hot spot left. Candidates that were measured or
estimated and not pursued are listed in NOTES.md ("Ideas not pursued yet").

## 4. Experiments behind design decisions

### Crossing cost (DESIGN.md §4)

`benches/exp_crossing.py`: the same traversal on the caller's Python objects
vs. on a converted Rust tree.

| document | direct | convert only | convert + work | ratio |
| --- | ---: | ---: | ---: | ---: |
| flat, 8 fields | 0.17 µs | 0.79 µs | 0.87 µs | 5.1× |
| nested, 8 levels | 0.32 µs | 1.45 µs | 1.61 µs | 5.0× |
| 1000 item dicts | 77 µs | 722 µs | 830 µs | 10.8× |
| 5000 ints | 31 µs | 70 µs | 81 µs | 2.6× |

### Regex (DESIGN.md §5)

`cargo bench -p ferberus-regex` (divan): a match costs 45–85 ns for the
typical patterns (date, e-mail, slug, UUID) against ~200 ns for a call into
`re` through the C-API; translating a pattern takes 1–5 µs and compiling it
15–35 µs (once per pattern, on first use).

### Releasing the GIL (DESIGN.md §7)

`benches/exp_gil_release.py`: one `regex` rule on subjects of growing
length; µs per validation with the interpreter held vs. released during the
match.

GIL build (3.14):

| subject bytes | 1 thread held | 1 thread released | 4 threads held | 4 threads released | gain |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 16 | 1.04 | 1.10 | 1.07 | 3.57 | 0.30× |
| 256 | 1.37 | 1.42 | 1.41 | 3.36 | 0.42× |
| 1024 | 2.41 | 2.63 | 2.90 | 4.17 | 0.69× |
| 2048 | 3.77 | 3.89 | 5.91 | 6.56 | 0.90× |
| 4096 | 6.63 | 6.73 | 16.33 | 4.65 | 3.52× |
| 16384 | 23.3 | 23.8 | 42.2 | 6.26 | 6.74× |
| 65536 | 90.7 | 90.2 | 197.5 | 23.4 | 8.45× |

Releasing costs ~0.1 µs per call and makes things worse for short subjects
(the threads fight over the GIL); it pays between 2 and 4 kB. Hence the
threshold of 4096 bytes. On the free-threaded build releasing makes no
difference (there is nothing to release); four threads with a validator each
process short documents at 1.9× and long ones at 3.9× the rate of one.

## 5. Regression tracking

CI runs `benches/run.py` and `benches/check_regression.py`, which compares
the *speedup over Cerberus measured in the same run* per workload with
`benches/results/baseline.json` and fails if one falls below 60 % of the
recorded value (GitHub's runners reach 75–105 % of the baseline machine's
speedups). Comparing ratios instead of times keeps the check meaningful
on runners with different hardware. Update the baseline deliberately
(`cp out.json benches/results/baseline.json`) when a change is meant to move
the numbers.
