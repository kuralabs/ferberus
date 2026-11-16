# Project: ferberus — a drop-in, high-performance Rust reimplementation of Cerberus

You are building **ferberus**, a Rust reimplementation of the Python validation
library Cerberus (https://github.com/pyeve/cerberus), exposed to Python via PyO3.
Work autonomously through the phases below. Keep a running `NOTES.md` with
decisions, measurements, and open questions.

## Goals, in priority order

1. **Exact compatibility (non-negotiable).** ferberus must be a drop-in
   replacement for Cerberus at the current `master` commit. Record the exact
   SHA in `NOTES.md` and pin to it. "Compatible" means identical observable
   behavior: return values, normalized documents (including key order),
   `errors` / `document_error_tree` / `schema_error_tree` structure, error
   message strings, error codes, exception types and messages, validator state
   after validation, and behavior with subclassed validators.
   - 100% of the upstream Cerberus test suite must pass, unmodified, against
     ferberus. Never edit an upstream test to make it pass. If you believe a
     test is wrong, document it in NOTES.md and stop on that item.
   - Add tests wherever the upstream suite leaves gaps (see "Verification").
2. **Speed.** This is the reason the project exists. Every design decision
   should be justified by measurements, not intuition.
3. **Modern Rust.** Latest stable Rust, edition 2024, PyO3 (latest), maturin,
   uv for Python tooling. Clean, idiomatic, `clippy -D warnings` clean.

## Phase -1 — Development environment (do this first)

All work happens inside a dev container. Do not install anything on the host.
The host provides only Docker (and the devcontainer CLI, if present). If
something truly requires a host change, stop and ask me.

- Create `.devcontainer/devcontainer.json` plus a `Dockerfile` (Debian/Ubuntu
  base). Build and work inside it, e.g. via `devcontainer up` /
  `devcontainer exec`, or plain `docker build` / `docker run` if the CLI is
  not available.
- The image must contain everything the project needs:
  - Rust stable via rustup, pinned through `rust-toolchain.toml`
    (with clippy, rustfmt, llvm-tools), plus cargo-nextest, cargo-deny,
    cargo-fuzz (nightly toolchain for fuzzing), criterion/divan deps.
  - uv, and every CPython version we support, **including free-threaded
    builds** (3.13t/3.14t), installed via uv.
  - maturin, Hypothesis, pytest, pytest-benchmark, ruff.
  - Profilers: py-spy, perf (linux-tools), samply or flamegraph tooling.
  - The pinned Cerberus checkout (for the reference implementation and the
    upstream test suite).
- Configure the container for profiling: `--cap-add=SYS_PTRACE` (py-spy) and
  whatever perf needs (`--cap-add=PERFMON` or `SYS_ADMIN`, and document any
  host `perf_event_paranoid` setting I need to apply).
- Use named volumes for the cargo registry, git cache, `target/` and the uv
  cache so rebuilding the container doesn't mean recompiling everything.
- Run as a non-root user with the workspace bind-mounted.

**Rules for changing the environment:**

- When you need a new tool or library, add it to the Dockerfile (pinned
  version), rebuild, and continue. Do not install anything ad hoc inside a
  running container. Anything not in the Dockerfile counts as missing.
- After each environment change, verify a clean rebuild from scratch works
  and the test suite still runs.
- Log every environment change and the reason for it in NOTES.md.
- CI must use the same image (or the same Dockerfile), so local and CI
  results match.
- Benchmarks always run inside the container on the same machine. Record
  the container image digest alongside each result in BENCHMARKS.md.

## Phase 0 — Understand and measure Cerberus first

- Clone Cerberus, run its test suite, and run its included benchmarks.
  Record baseline numbers on this machine.
- Extend the benchmarks to cover realistic workloads: small flat docs, deep
  nesting, large lists (`schema` on list items), `anyof`/`oneof`, heavy
  normalization (`coerce`, `default`, `rename`, `purge_unknown`), `regex`,
  schema validation itself, and validator construction cost. Include both
  "validate many small docs with one validator" and "one huge document".
- Profile Cerberus (py-spy and cProfile; flamegraphs) and write up where the
  time goes. This guides what to optimize first.

## Phase 1 — Map the full public surface

Before writing core code, produce `SURFACE.md` covering everything users can
touch, including:
- `Validator` and its constructor args/properties (`allow_unknown`,
  `require_all`, `purge_unknown`, `purge_readonly`, `ignore_none_values`,
  `error_handler`, `schema`, `document`, `errors`, `recent_error`, etc.)
- Every built-in rule and normalization rule, `*of` rules and type-savers
  (e.g. `anyof_type`), dependencies, `excludes`, `readonly`, `nullable`,
  `meta`, `check_with`, `coerce` chains, `default_setter`, `rename_handler`.
- Extension points: subclassing with `_validate_<rule>`,
  `_check_with_<name>`, `_normalize_coerce_<name>`,
  `_normalize_default_setter_<name>`, `types_mapping` / `TypeDefinition`,
  **docstring-embedded rule schemas**, `validator_factory`.
- Registries (`schema_registry`, `rules_set_registry`), error handlers
  (`BasicErrorHandler`, custom handlers), `ValidationError`, error trees,
  `SchemaError`, `DocumentError`.
- Python-level behaviors: Mapping/Sequence subclasses, `set`, `datetime`,
  `Decimal`, custom types, pickling, copying, reuse of a validator across
  calls, thread usage.

## Phase 2 — Architecture (write `DESIGN.md` before implementing)

Key constraints to solve explicitly:
- **Hybrid execution.** Built-in rules run entirely in Rust. Custom rules on
  Python subclasses are called back into Python only where they appear. A
  schema made only of built-in rules must never touch the Python
  interpreter in the hot path beyond reading input.
- **Crossing cost.** Measure whether to convert input into Rust structures
  or read Python objects directly. Avoid deep copies unless normalization
  needs them. Prove the choice with benchmarks.
- **Schema compilation.** Compile and cache schemas into an optimized
  internal form at validator construction; validation should not re-interpret
  the schema dict on each call.
- **Regex.** Rust's `regex` crate is not compatible with Python `re`
  (backreferences, lookaround, flag/anchor semantics). Choose a strategy
  (`regex` fast path + `fancy-regex` + Python `re` fallback, or similar) and
  prove that matching behavior is identical.
- **Python API layer.** Keep a thin Python package where needed for exact
  compatibility (subclassing, docstring parsing, error-handler classes), with
  the Rust core underneath. Provide `import ferberus` as the primary name, and
  a way to run code that does `import cerberus` against ferberus (shim
  package or import alias) so the upstream suite runs unmodified.
- **Concurrency.** Correct under the GIL and under free-threaded CPython
  (3.13t/3.14t). Release the GIL where it's safe and it helps.

## Verification

- Upstream Cerberus suite: 100% passing, unmodified, in CI.
- **Differential testing**: Hypothesis-based property tests that generate
  random schemas and documents, run both real Cerberus and ferberus, and
  assert identical results (validity, errors, error trees, normalized output,
  exceptions). Run long fuzzing sessions and record each discrepancy found
  as a permanent regression test.
- Rust unit tests (cargo-nextest), and Rust-level fuzzing (cargo-fuzz) for
  the core where useful.
- Any intentional deviation from Cerberus must be listed in
  `COMPATIBILITY.md`. The target is zero.

## Performance reporting

- Rust-side micro-benchmarks with criterion or divan; Python-side benchmarks
  comparing Cerberus vs ferberus on the Phase 0 workloads.
- Keep a results table in `BENCHMARKS.md` (speedup per workload, with
  hardware, Python version, commit). Profile ferberus too (perf/samply) and
  iterate on the largest costs.
- Track performance regressions in CI.

## Tooling and packaging

- maturin build, abi3 wheels where possible, supported Python versions
  matching Cerberus (state them), wheels for Linux (manylinux + musl,
  x86_64 + aarch64), macOS (x86_64 + arm64), Windows.
- rustfmt, clippy (`-D warnings`), cargo-deny, ruff for Python code,
  GitHub Actions CI.
- License: preserve Cerberus's ISC license notice and attribute the
  original project.

## Working style

- Work in milestones: (1) baseline + surface map, (2) design, (3) core rules
  passing a growing share of the upstream suite, (4) normalization,
  (5) extension points, (6) differential fuzzing clean, (7) performance work.
  Report pass rate and speedups at each milestone.
- Correctness first, then speed. Never trade compatibility for performance.
  If a fast path can't be proven identical, gate it behind a fallback.
- When you can't decide something from the code, the tests, or
  measurements, write the question in NOTES.md and continue with other
  work.
