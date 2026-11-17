# Contributing to ferberus

## Development environment

Everything runs in the dev container (`.devcontainer/`); the host only needs
Docker. Changes to the environment go into the Dockerfile and are logged in
[NOTES.md](NOTES.md).

```console
$ ./dev build                      # build the image
$ ./dev scripts/check.sh           # build, upstream suite, differential tests
$ ./dev scripts/ci.sh              # everything CI runs
$ ./dev python benches/run.py      # Cerberus vs ferberus
$ ./dev scripts/soak.sh 100000     # long fuzzing session
```

Compatibility with Cerberus comes first: the unmodified upstream test suite
must pass on every supported interpreter, and upstream tests are never
edited. A behavior that differs from Cerberus is a bug unless it is listed
in [COMPATIBILITY.md](COMPATIBILITY.md). Performance changes are justified by
measurements ([BENCHMARKS.md](BENCHMARKS.md)).

## Releasing

1. Set `version` in `Cargo.toml` (the Python package takes it from there)
   and commit.
2. Tag and push: `git tag -a vX.Y.Z -m "ferberus X.Y.Z"` and
   `git push origin main vX.Y.Z`.
3. The `Wheels` workflow builds 63 wheels and the sdist, runs the upstream
   suite against every wheel, checks that all files carry the tag's version
   and publishes them to PyPI (Trusted Publishing, environment `pypi`).

## Inspecting what the engine did

```python
from ferberus import _core

_core.reset_stats(True)
...  # validate things
_core.stats()  # native / delegated / declined, with reasons
_core.set_enabled(False)  # reference implementation only
```

## Documentation of this repository

| File | Content |
| --- | --- |
| [SURFACE.md](SURFACE.md) | The public surface of Cerberus that ferberus reproduces. |
| [DESIGN.md](DESIGN.md) | Architecture and the measurements behind it. |
| [COMPATIBILITY.md](COMPATIBILITY.md) | Verification and every known deviation. |
| [BENCHMARKS.md](BENCHMARKS.md) | Results, hardware, methodology. |
| [NOTES.md](NOTES.md) | Working log: decisions, environment changes, open questions. |
| [ROADMAP.md](ROADMAP.md) | What gated 1.0.0 and what comes after. |

The short benchmark table in README.md is generated with
`./dev python benches/report.py --readme benches/results/cp314.json`.
