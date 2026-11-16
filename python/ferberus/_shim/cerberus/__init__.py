"""
Extensible validation for Python dictionaries.

This is the ``cerberus`` compatibility shim of ferberus: every name resolves to
the corresponding ferberus object. It is only importable after
:func:`ferberus.compat.install` was called. The module body deliberately
mirrors ``cerberus/__init__.py``.
"""

import sys

import ferberus.errors
import ferberus.platform
import ferberus.schema
import ferberus.utils
import ferberus.validator

for _name in ("errors", "platform", "schema", "utils", "validator"):
    sys.modules[__name__ + "." + _name] = sys.modules["ferberus." + _name]
    globals()[_name] = sys.modules["ferberus." + _name]
del _name

from cerberus.platform import importlib_metadata  # noqa: E402
from cerberus.schema import rules_set_registry, schema_registry, SchemaError  # noqa: E402
from cerberus.utils import TypeDefinition  # noqa: E402
from cerberus.validator import DocumentError, Validator  # noqa: E402

try:
    __version__ = importlib_metadata.version("Cerberus")
except importlib_metadata.PackageNotFoundError:
    __version__ = "unknown"

__all__ = [
    DocumentError.__name__,
    SchemaError.__name__,
    TypeDefinition.__name__,
    Validator.__name__,
    "schema_registry",
    "rules_set_registry",
    "__version__",
]
