# Roadmap

## 1.0.0

ferberus is feature-complete with respect to Cerberus 1.3.x: every rule,
extension point and public class is there, and the reference implementation
covers whatever the native engine declines. `1.0.0-rc.1` became `1.0.0`
on 2026-11-17, after it had passed these gates:

| Gate | State |
| --- | --- |
| Upstream suite, documentation doctests and all tests green on every supported interpreter (`scripts/ci.sh`) | passed locally in the dev container on all nine interpreters (`1.0.0-rc.1`) |
| A long differential fuzzing session on the release candidate without findings (`scripts/soak.sh 100000`) | passed: 100 000 examples per property clean on `1.0.0-rc.1` (five sessions of 20 000) |
| `CI` workflow green on GitHub | passed (run 44032698976, 35 min) |
| `Wheels` workflow: manylinux and musllinux (x86_64, aarch64), macOS (x86_64, arm64), Windows; the wheels pass the upstream suite | passed (run 44037196201): nine wheels per platform; the upstream suite passes on every interpreter on Linux x86_64 and aarch64, macOS arm64 and x86_64, and Windows. The musllinux wheels are built but not run |

The source of `1.0.0` is identical to `1.0.0-rc.1` apart from the version
number.

**Publishing 1.0.0** is manual: KuraLabs uploads the wheels and the sdist
that the `Wheels` workflow builds for the `v1.0.0` tag (the workflow's
artifacts) to PyPI. Done on 2026-11-17:
<https://pypi.org/project/ferberus/1.0.0/>, 63 wheels and the sdist. The
upstream suite passes (246 passed, 1 skipped) against the wheels installed
from PyPI (`pip install --only-binary=:all: ferberus==1.0.0`) on all nine
interpreters in the dev container.

**Known issue in 1.0.0:** the cp39 and cp310 musllinux wheels do not import
on interpreters installed by uv (python-build-standalone); official CPython
builds on Alpine are fine. Fixed on `main` (`scripts/musl-suffix.py`, see
NOTES.md), shipped with the next release.

## After 1.0.0

Performance, in the order of expected benefit:

1. **Schema validation and validator construction** (currently 2–3× faster
   than Cerberus, everything else 20–180×). The schema validator's checks
   are Python hooks; a native implementation has to reproduce the error
   dicts of `SchemaError` exactly. Also: expanding schemas natively, and
   skipping the hash of a schema that is known by identity.
2. **Cheaper mutation guard.** Dict watchers (CPython ≥ 3.12) can replace
   the per-call pointer comparison of the schema (~10 % of a small
   validation) by an epoch check.
3. **Free-threaded scaling.** Four threads with a validator each process
   small documents at 1.9× the rate of one (3.9× for large documents);
   shared reference counts and the per-call allocations are the suspects.
4. **Per-call overhead**: assembling the validator's state for a call
   (configuration lookups, error list, sets) is ~20 % of a small validation.
5. **Hooks**: cheaper calls into `check_with` functions and rule methods
   (prepared method lookups, vectorcall with keyword-free signatures).

Release engineering:

6. **Publish from CI.** In place since 2026-11-17, first used by the next
   release: the `publish` job of the `Wheels` workflow uploads to PyPI
   when a `v*` tag is pushed, after every `test` leg passed, with PyPI
   Trusted Publishing (OIDC, no stored token). The `pypi` environment only
   deploys `v*` tags; it has no required reviewers yet.

Scope:

7. **PyPy.** Cerberus supports it; the native engine relies on CPython's
   C-API. Options: ship the reference implementation alone for PyPy, or a
   cpyext build.
8. **abi3 wheels** as an alternative build (slower, one wheel per
   platform), if the number of wheels becomes a burden.
9. **Following upstream.** A procedure for moving the pin to a newer
   Cerberus commit: re-port the Python layer, run both suites and the
   fuzzer.
