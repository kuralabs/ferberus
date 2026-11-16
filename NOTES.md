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
  scope for now (see Q2).
- **D2 – documents are read as Python objects, not converted.** See DESIGN.md.
- **D3 – the Python layer is a port of Cerberus' own modules** (ISC, attributed
  in `LICENSE.cerberus`); it is the semantic reference inside ferberus and the
  fallback engine. The Rust engine takes over whenever it can prove
  equivalence. See DESIGN.md.

## Open questions

- **Q1 – 3.13t.** PROMPT.md asks for 3.13t and "PyO3 (latest)". PyO3 0.30
  refuses to build for free-threaded CPython < 3.14 ("PyO3 does not support
  the free-threaded build of CPython versions below 3.14"). Going with latest
  PyO3 and 3.14t/3.15t; 3.13t would need PyO3 <= 0.2x. To be confirmed with
  our own crate (the message above came from Hypothesis' build).
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
