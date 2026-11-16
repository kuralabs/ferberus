"""
ferberus: a drop-in, high-performance reimplementation of Cerberus.

Extensible validation for Python dictionaries. The public API mirrors
Cerberus 1.3.x (https://python-cerberus.org/), from which the Python layer of
this package is derived.

:copyright: 2012-2023 by Nicola Iarocci (Cerberus); ferberus contributors.
:license: ISC, see LICENSE for more details.
"""

from ferberus.platform import importlib_metadata
from ferberus.schema import rules_set_registry, schema_registry, SchemaError
from ferberus.utils import TypeDefinition
from ferberus.validator import DocumentError, Validator

try:
    __version__ = importlib_metadata.version("ferberus")
except importlib_metadata.PackageNotFoundError:
    __version__ = "unknown"

#: The Cerberus release line whose behavior ferberus reproduces.
__cerberus_version__ = "1.3.8"

__all__ = [
    DocumentError.__name__,
    SchemaError.__name__,
    TypeDefinition.__name__,
    Validator.__name__,
    "schema_registry",
    "rules_set_registry",
    "__version__",
]
