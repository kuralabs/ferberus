# ferberus — working notes

Running log of decisions, measurements and open questions. Newest sections
are appended at the bottom of each chapter.

## Pinned reference

| What | Value |
| --- | --- |
| Upstream | https://github.com/pyeve/cerberus |
| Branch | `1.3.x` (GitHub default branch; the line released on PyPI) |
| Commit | `83f5dada76aa3179ef21b677a7f4235bf20c1566` (2026-10-01) |
| Version | 1.3.8 (the branch tip differs from tag `1.3.8` only by 8 deleted lines of comments/py2 leftovers under `cerberus/`) |

`PROMPT.md` says "current `master`". Upstream `master` (`8765b31`, last commit
2021-05-05) is an abandoned 2.0-dev branch with a different API that was never
released. Carlos confirmed on 2026-11-16 that the target is **`1.3.x` HEAD**.

The SHA is pinned in `.devcontainer/Dockerfile` (`CERBERUS_SHA`); the image
build fails if the checkout does not resolve to it. The checkout lives at
`/opt/cerberus` inside the container.

## Environment log

All work happens in the dev container (`.devcontainer/`). `./dev` is a plain
docker wrapper equivalent to `devcontainer.json` (the devcontainer CLI is not
installed on the host).

| Date | Change | Reason |
| --- | --- | --- |
| 2026-11-16 | Initial image: `debian:trixie-20261104-slim`, rustup 1.29.3, Rust 1.100.0 (+clippy, rustfmt, llvm-tools, rust-src), nightly-2026-11-10 (cargo-fuzz), cargo-binstall 1.27.0, cargo-nextest 0.9.152, cargo-deny 0.20.2, cargo-fuzz 0.13.2, samply 0.13.1, flamegraph 0.6.15, uv 0.13.8, CPython 3.9–3.15 + 3.13t/3.14t/3.15t, maturin 1.16.0, pytest 9.2.0, pytest-benchmark 5.3.1, hypothesis 6.170.15, ruff 0.17.5, py-spy 0.4.3, linux-perf 6.12, gdb. | Phase -1. |
| 2026-11-16 | `PATH` in the image includes the `sbin` directories. | `setcap`/`groupadd` were not found during the first builds. |
| 2026-11-16 | 3.9/3.10 venvs use `requirements-legacy.txt` (pytest 8.4.2, hypothesis 6.141.1); 3.13t uses `requirements-minimal.txt` (no Hypothesis). | Current pytest needs >= 3.10, Hypothesis >= 3.11; Hypothesis' native extension does not build for free-threaded 3.13. |
| 2026-11-16 | 3.13t interpreter and `requirements-minimal.txt` removed. | PyO3 0.30 does not build for free-threaded 3.13; 3.14t is the agreed floor (D1). |
| 2026-11-16 | `requirements-docs.txt` (sphinx 9.2.0, furo 2025.12.19, PyYAML 6.0.3) installed into the 3.14 venv. | `scripts/upstream-doctests.sh`: the 287 doctests of Cerberus' documentation run against ferberus (upstream's `tox -e doctest`). |

After the last change the image was rebuilt from scratch (`./dev build
--no-cache`, digest `sha256:7fa2be0f488c…8da5`), the container recreated and
`scripts/ci.sh lint rust python` run in it.

Image facts:

- Non-root user `dev` (uid/gid taken from the host user at build time by
  `./dev build`; `updateRemoteUserUID` does the same for the devcontainer CLI).
- Named volumes: `ferberus-cargo-registry`, `ferberus-cargo-git`,
  `ferberus-target` (`CARGO_TARGET_DIR=/var/cache/ferberus/target`, outside
  the bind mount), `ferberus-uv-cache`.
- Profiling: the container runs with `--cap-add=SYS_PTRACE --cap-add=PERFMON`.
  `perf` carries file capabilities (`cap_perfmon,cap_sys_ptrace+ep`) so the
  unprivileged user can use it. Verified working with the host at
  `kernel.perf_event_paranoid=4` (Ubuntu default) — **no host change needed**.
  `py-spy record -- <cmd>` works as well. Should kernel symbols be wanted in
  perf output, the host needs `sudo sysctl kernel.kptr_restrict=0` and
  `kernel.perf_event_paranoid<=1`; not required for anything done so far.
- Interpreters are uv-managed (`/opt/python`), one venv each under
  `/opt/venvs/<version>`; `/opt/venvs/3.14/bin` is first on `PATH`.
- Real Cerberus is *not* installed into the venvs. The reference is put on
  `sys.path` explicitly (`/opt/cerberus`) by the differential tests and
  benchmarks, so `import cerberus` can be aliased to ferberus for the upstream
  suite.

## Decisions

- **D1 – supported interpreters.** CPython 3.9–3.15, plus free-threaded
  3.14t/3.15t. Cerberus declares `>=3.7` and tests 3.8–3.14 + PyPy. PyO3 0.30
  requires CPython >= 3.9, so 3.7/3.8 cannot be supported; PyPy is out of
  scope for now (see Q2). 3.14t as the floor for free-threaded builds was
  confirmed by Carlos on 2026-11-16 (PyO3 0.30 rejects 3.13t; verified with
  our own crate).
- **D2 – documents are read as Python objects, not converted.** See DESIGN.md.
- **D3 – the Python layer is a port of Cerberus' own modules** (ISC, attributed
  in `LICENSE.cerberus`); it is the semantic reference inside ferberus and the
  fallback engine. The Rust engine takes over whenever it can prove
  equivalence. See DESIGN.md.

- **D4 – no abi3.** The hot path uses `PyDict_Next`, `PyList_GET_ITEM`,
  `PyFloat_AS_DOUBLE`, type pointers and `PyObject_GenericGetDict`; wheels
  are built per interpreter version.
- **D5 – regex.** `regex` crate fast path for a translated subset + Python
  `re` for everything else; no `fancy-regex` (DESIGN.md §5).
- **D6 – child validators only where Python needs them** (DESIGN.md §2.2);
  the resulting deviation (how often a subclass' `__init__` runs) is listed
  in COMPATIBILITY.md.
- **D7 – no native method for `Validator.validate`.** Binding the entry
  point as a C-level method would save ~0.3 µs per call but changes what
  introspection sees (`__code__`, signatures, `mock.patch(autospec=True)`).
  The methods stay Python functions that call into the core.
- **D8 – commits.** Committing is allowed (Carlos, 2026-11-16).

- **D9 – release.** Copyright holder is KuraLabs S.R.L. The remaining
  performance ideas are post-1.0 (Carlos, 2026-11-16); the gates for 1.0.0
  are in ROADMAP.md, the version is `1.0.0-rc.1` until they are passed.
  Work moved from branch `engine` to `main`. 1.0.0 is published to PyPI manually by
  Carlos; publishing from CI is a post-1.0 roadmap item.

## Ideas not pursued yet

(Planned order: ROADMAP.md.)

- **Dict watchers for the mutation guard** (CPython >= 3.12,
  `PyDict_Watch`): would turn the per-call guard check (~10 % of a small
  validation) into an epoch comparison plus the checks of the few lists and
  sets in a schema.
- **Schema validation in Rust.** Validator construction is 2.6–2.9× faster
  (native `mapping_hash`, lazy schema validator), schema validation itself
  2.3× (the schema validator's hooks are still Python). A native
  implementation of `SchemaValidatorMixin` would have to reproduce the
  exact error dicts of `SchemaError`.
- **PyPy** (Q2).

## Open questions

- **Q1 – 3.13t.** *Resolved:* 3.14t is the floor (see D1).
- **Q2 – PyPy.** Cerberus supports PyPy; PyO3 supports PyPy 7.3 (3.11+). The
  native engine relies on CPython C-API details. Not attempted yet.

## Baseline (Phase 0)

Machine: Intel Core i7-8700K (6C/12T, 3.7 GHz), 64 GB RAM, Linux 7.0.0-43,
Docker 29.9.3. Image `sha256:2faa93fe00ab…e65b`.

- Upstream suite against real Cerberus: `246 passed, 1 skipped` on 3.9, 3.14,
  3.15 and 3.14t.
- Upstream bundled benchmarks, CPython 3.14.10, real Cerberus:
  `test_overall_performance_1` min 8.158 s / mean 8.183 s;
  `test_overall_performance_2` min 8.481 s / mean 8.544 s (5 rounds each).

## Milestone log

Pass rate = upstream suite against ferberus (246 tests + 1 skip upstream).
Speedups = `benches/run.py`, CPython 3.14.10, vs real Cerberus on this machine.

| # | Milestone | Upstream suite | Speedup (selected) |
| --- | --- | --- | --- |
| 1 | Baseline, surface map (`SURFACE.md`), pure-Python port + `cerberus` alias | 246/246 | 1.0× (port) |
| 2 | Design (`DESIGN.md`), crossing-cost experiment | 246/246 | – |
| 3–4 | Native engine: all built-in validation and normalization rules, native frames, delegation of unsupported sub-schemas | 246/246 | small flat docs 103×, list of dicts 51×, normalization 73×, `*of` 50× |
| 5a | Hooks on the root validator (custom rules, `check_with`, named coercers/setters, legacy type methods), registry references, mutation guard | 246/246 | custom rules 7.9×, upstream benchmark 1: 4.3× |
| 5b | Frames with real child validators for hooks at nested levels | 246/246 | custom rules 16.7×, upstream benchmark 1: 14×, benchmark 2: 81× |
| 6 | Differential fuzzing: 5 properties × 2000 examples clean on every interpreter; long sessions, see below | 246/246 | – |
| 7 | Performance work: whole call in Rust, native `errors` rendering, regex fast path, natively assembled child validators, child/schema validators in the engine, native `mapping_hash` | 246/246 | see BENCHMARKS.md |

### Discrepancies found by the differential fuzzer (all fixed, all are upstream quirks)

1. After an exception inside a rule, `_remaining_rules` keeps the unprocessed
   rules of that field (and they stay until the next field is processed).
2. `_validate_min` / `_validate_max` call `self._error` inside their
   `try: ... except TypeError: pass`, so a `TypeError` raised while *filing*
   the error (sorting errors whose paths contain unorderable keys) is
   swallowed; the error is in `_errors` but not in the trees.
3. A field named `None` does not extend the document path of its child
   validator (`document_crumb=None` means "no crumb"); a tuple field name is
   spliced into the path.
4. Adding errors to the error trees can raise (`TypeError` from
   `compare_paths_lt`) where sorting `_errors` did not, because the tree
   nodes sort different subsets. Lazy trees are therefore only kept while
   all error paths are plain `str`/`int` tuples.
5. Nested `*of` definitions inherit the `allow_unknown` that the *outer*
   definition was filled in with, not the evaluating child's (`True`).
6. Expanding a schema replaces the lists of `*of`/`items` rules with new,
   equal lists — on every normalization. The mutation guard treats a
   replaced list with identical members as unchanged.
7. In a field whose rules queue lives on the validator (because of hooks),
   `empty` drops rules before it files its error; if filing raises, the
   rules stay dropped.
8. `BasicErrorHandler` formats a deep copy of each error, and a deep-copied
   set can iterate in another order than the original: the text of
   `missing members {...}` depends on it.
9. The normalized value of a container field is assigned *before* the
   errors of its child validator are filed; if filing raises, the document
   keeps the new value.
10. `KeyError(None)` (renaming a field named `None` twice) has the message
    `'None'`; an exception created from a bare `None` argument has none.
11. (Found by the gap tests.) An instance attribute shadowing a rule method
    went unnoticed if the instance had not validated anything yet: the
    count of instance attributes then matched the usual one.

### GitHub workflows (2026-11-17)

- `CI`: green on the first complete run (image build, lint, Rust tests and
  fuzz smoke test, all nine interpreters, performance check; 35 min).
- `Wheels`: three runs. (1) `macos-13` never left the queue and `macos-14`
  was cancelled before it started → `macos-15-intel` / `macos-latest`.
  (2) All green, but the Windows job had silently built only five of nine
  wheels (`--find-interpreter` missed 3.9, 3.10 and the free-threaded
  builds) and only the Linux x86_64 wheels were tested → interpreters are
  passed explicitly, the test job runs the upstream suite on every platform
  and insists on nine wheels. (3) Green.
- Runs are watched and dispatched through the REST API with a token Carlos
  provided for the session (not stored anywhere in the repository). The token was
  revoked on 2026-11-17; watching runs from now on is limited to the
  unauthenticated API (status, no logs).

### Long fuzzing sessions

| Date | Build | Session | Result |
| --- | --- | --- | --- |
| 2026-11-16 | before commit `3da3b97` | 100 000 examples/property requested, cut off after 1 h | findings 8 and 9 |
| 2026-11-16 | commit `8792d8c` | regex properties, 30 000 examples each on 3.9, 3.12, 3.14 | clean, after two findings at 2000–20 000 examples: `\B` on an empty subject depends on the Python version (matches only since 3.14), `(?a)` removes 0x1c–0x1f from `\s`; both constructs are now left to `re` |
| 2026-11-16 | `1.0.0-rc.1` (`4a6b384`, generator fix `HEAD`) | 20 000 examples for each of the 5 differential and 3 regex properties, CPython 3.14 | clean (two interruptions were bugs of the test generator on junk schemas, fixed) |
| 2026-11-17 | `1.0.0-rc.1` (`72baca1`) | second session, 20 000 examples for each of the 8 properties, CPython 3.14 | clean (40 000 per property in total) |
| 2026-11-17 | `1.0.0-rc.1` (`72baca1`) | third session, 20 000 examples for each of the 8 properties, CPython 3.14 | clean (60 000 per property in total) |
