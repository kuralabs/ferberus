# ferberus

A drop-in, high-performance reimplementation of
[Cerberus](https://github.com/pyeve/cerberus), the Python validation library,
with a Rust core (PyO3).

```python
from ferberus import Validator  # same API as cerberus

v = Validator({"name": {"type": "string"}, "age": {"type": "integer", "min": 10}})
v.validate({"name": "Little Joe", "age": 5})  # False
v.errors  # {'age': ['min value is 10']}
```

- **Compatible.** Behaves like Cerberus 1.3.x (pinned at `83f5dad`): results,
  normalized documents, `errors`, error trees, messages, exceptions,
  subclassing with custom rules, registries. The unmodified Cerberus test
  suite passes, and a differential fuzzer compares both implementations.
  Known differences are listed in [COMPATIBILITY.md](COMPATIBILITY.md).
- **Fast.** Typically 50–150× faster for validation and normalization; see
  [BENCHMARKS.md](BENCHMARKS.md).
- **Hybrid.** Built-in rules run natively. Custom rules, checks and coercers
  of your `Validator` subclasses are called back into Python exactly where
  they appear. Anything the native engine cannot reproduce exactly is run
  by the bundled reference implementation (a port of Cerberus itself).

## Using it in place of Cerberus

```python
import ferberus.compat

ferberus.compat.install()  # before anything imports cerberus

import cerberus  # this is ferberus now
```

`cerberus.errors`, `cerberus.schema`, `cerberus.utils`, `cerberus.validator`
and `cerberus.platform` resolve to the ferberus modules. For a test suite:
`pytest -p ferberus.compat`.

Requirements: CPython 3.9–3.15, including free-threaded 3.14t/3.15t.

## Documentation of this repository

| File | Content |
| --- | --- |
| [SURFACE.md](SURFACE.md) | The public surface of Cerberus that ferberus reproduces. |
| [DESIGN.md](DESIGN.md) | Architecture and the measurements behind it. |
| [COMPATIBILITY.md](COMPATIBILITY.md) | Verification and every known deviation. |
| [BENCHMARKS.md](BENCHMARKS.md) | Results, hardware, methodology. |
| [NOTES.md](NOTES.md) | Working log: decisions, environment changes, open questions. |
| [ROADMAP.md](ROADMAP.md) | What gates 1.0.0 and what comes after. |

## Development

Everything runs in the dev container (`.devcontainer/`); the host only needs
Docker.

```console
$ ./dev build                      # build the image
$ ./dev scripts/check.sh           # build, upstream suite, differential tests
$ ./dev scripts/ci.sh              # everything CI runs
$ ./dev python benches/run.py      # Cerberus vs ferberus
$ ./dev scripts/soak.sh 100000     # long fuzzing session
```

Inspecting what the engine did:

```python
from ferberus import _core

_core.reset_stats(True)
...  # validate things
_core.stats()  # native / delegated / declined, with reasons
_core.set_enabled(False)  # reference implementation only
```

## License

ISC, like Cerberus. © KuraLabs S.R.L. ferberus' Python layer is derived from
Cerberus (© Nicola Iarocci and contributors); see [LICENSE](LICENSE) and
[LICENSE.cerberus](LICENSE.cerberus).
