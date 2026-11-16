# Roadmap

## 1.0.0

ferberus is feature-complete with respect to Cerberus 1.3.x: every rule,
extension point and public class is there, and the reference implementation
covers whatever the native engine declines. The current version is
`1.0.0-rc.1`. It becomes `1.0.0` when these gates are passed:

| Gate | State |
| --- | --- |
| Upstream suite, documentation doctests and all tests green on every supported interpreter (`scripts/ci.sh`) | passes locally in the dev container |
| A long differential fuzzing session on the release candidate without findings (`scripts/soak.sh 100000`) | to do for each candidate; every finding becomes a regression test and restarts the gate |
| `CI` workflow green on GitHub | written, never run (no remote yet) |
| `Wheels` workflow: manylinux and musllinux (x86_64, aarch64), macOS (x86_64, arm64), Windows; the wheels pass the upstream suite | written, only the Linux x86_64 wheel was built and tested locally |

No further engine work is planned before 1.0.0; only fixes for what the
gates turn up.

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

Scope:

6. **PyPy.** Cerberus supports it; the native engine relies on CPython's
   C-API. Options: ship the reference implementation alone for PyPy, or a
   cpyext build.
7. **abi3 wheels** as an alternative build (slower, one wheel per
   platform), if the number of wheels becomes a burden.
8. **Following upstream.** A procedure for moving the pin to a newer
   Cerberus commit: re-port the Python layer, run both suites and the
   fuzzer.
