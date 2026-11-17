# ferberus

[![PyPI](https://img.shields.io/pypi/v/ferberus)](https://pypi.org/project/ferberus/)
[![Python versions](https://img.shields.io/pypi/pyversions/ferberus)](https://pypi.org/project/ferberus/)
[![CI](https://github.com/kuralabs/ferberus/actions/workflows/ci.yml/badge.svg)](https://github.com/kuralabs/ferberus/actions/workflows/ci.yml)
[![License: ISC](https://img.shields.io/pypi/l/ferberus)](https://github.com/kuralabs/ferberus/blob/main/LICENSE)

[Cerberus](https://github.com/pyeve/cerberus), the Python validation library,
reimplemented with a Rust core: the same API and the same results, typically
50–150× faster.

## Install

```console
$ pip install ferberus
```

Precompiled wheels are available for Linux, macOS and Windows (see
Platforms below); no Rust toolchain is needed to install them.

## Quick start

New code uses ferberus like Cerberus:

```python
from ferberus import Validator

v = Validator({"name": {"type": "string"}, "age": {"type": "integer", "min": 10}})
v.validate({"name": "Little Joe", "age": 5})  # False
v.errors  # {'age': ['min value is 10']}
```

The [Cerberus documentation](https://docs.python-cerberus.org/)
applies as it is: schemas, rules, normalization, error handlers, custom
validators and registries all work the same.

Existing code that does `import cerberus` can run on ferberus without
changes. Install the alias once, before anything imports `cerberus`:

```python
import ferberus.compat

ferberus.compat.install()

import cerberus  # this is ferberus now
```

`cerberus.errors`, `cerberus.schema`, `cerberus.utils`, `cerberus.validator`
and `cerberus.platform` then resolve to the ferberus modules. To run a test
suite against ferberus, use the pytest plugin instead:
`pytest -p ferberus.compat`.

## How fast

| Operation | Cerberus | ferberus | Speedup |
| --- | ---: | ---: | ---: |
| Validate a small document (8 fields) | 301.3 µs | 1.8 µs | **165×** |
| … invalid, 4 errors, reading `v.errors` | 424.7 µs | 8.2 µs | **52×** |
| Validate a document nested 8 levels deep | 486.4 µs | 5.2 µs | **93×** |
| Normalize a document (coerce, default, rename, purge) | 455.5 µs | 5.3 µs | **86×** |
| Validate a list of 1000 dicts | 73.65 ms | 1.05 ms | **70×** |
| Subclass with a custom rule, check and coercer | 248.4 µs | 6.7 µs | **37×** |
| Create a `Validator` (8-field schema) | 65.8 µs | 22.8 µs | **2.9×** |

These are times per operation, measured on CPython 3.14 on an Intel Core
i7-8700K, with Cerberus 1.3.8 as the reference. Creating a validator is the
slow part, so create one per schema and reuse it. All workloads, other
Python versions, free-threaded results and the method are in
[BENCHMARKS.md](https://github.com/kuralabs/ferberus/blob/main/BENCHMARKS.md).

## Platforms

Wheels on PyPI:

| OS | Architectures | Minimum version |
| --- | --- | --- |
| Linux (glibc) | x86_64, aarch64 | glibc 2.28 (manylinux_2_28) |
| Linux (musl, e.g. Alpine) | x86_64, aarch64 | musl 1.2 (musllinux_1_2) |
| macOS | Intel, Apple Silicon | macOS 10.12 (Intel), 11.0 (Apple Silicon) |
| Windows | x64 | — |

Each of them is built for CPython 3.9, 3.10, 3.11, 3.12, 3.13, 3.14 and 3.15,
plus the free-threaded builds 3.14t and 3.15t.

Before every release, the Cerberus test suite is run against every built
wheel: on Linux (glibc and, in Alpine containers, musl), macOS and Windows.

On any other platform, pip builds ferberus from the source distribution,
which needs Rust 1.100 or newer. PyPy is not supported.

## Compatibility

ferberus behaves like Cerberus 1.3.x (pinned at commit `83f5dad`): the same
validation results, normalized documents, `errors`, error trees, messages
and exceptions. This is checked by:

- the unmodified Cerberus test suite, run on every supported interpreter,
  and the doctests of the Cerberus documentation;
- a differential fuzzer, which generates schemas and documents and compares
  everything observable between Cerberus and ferberus.

The few known differences are about edge cases, such as how often
user-defined special methods (`__eq__`, `__hash__`, …) are called. They are
listed in
[COMPATIBILITY.md](https://github.com/kuralabs/ferberus/blob/main/COMPATIBILITY.md).

## How it works and getting the most out of it

Schemas are compiled once and validated by native code. Anything that the
native engine cannot reproduce exactly is run by a bundled Python port of
Cerberus itself, so every schema and every subclass works, though not all of
them at native speed:

- **Custom rules, checks and coercers** of your `Validator` subclasses are
  called back into Python, only for the fields that use them. Everything
  else stays native.
- **Overriding engine internals** in a subclass (`_error`,
  `_get_child_validator`, `_lookup_field`, …) makes that class run entirely
  on the Python port, at about the speed of Cerberus.
- **Unusual schemas**, such as references that cannot be resolved or
  mappings that are not dicts, are also run on the Python port, but only
  the smallest part of the schema that contains them.
- **Reuse validators.** Compiling a schema is the expensive step. Changing a
  validator's schema after it has been created is supported, but it makes
  ferberus compile the schema again.

The architecture is described in
[DESIGN.md](https://github.com/kuralabs/ferberus/blob/main/DESIGN.md).

## Contributing

See
[CONTRIBUTING.md](https://github.com/kuralabs/ferberus/blob/main/CONTRIBUTING.md).

## License

ISC, like Cerberus. © KuraLabs S.R.L. The Python layer of ferberus is
derived from Cerberus (© Nicola Iarocci and contributors); see
[LICENSE](https://github.com/kuralabs/ferberus/blob/main/LICENSE) and
[LICENSE.cerberus](https://github.com/kuralabs/ferberus/blob/main/LICENSE.cerberus).
