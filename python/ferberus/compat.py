"""
Run code that does ``import cerberus`` against ferberus.

    import ferberus.compat
    ferberus.compat.install()
    import cerberus          # -> ferberus

``cerberus.errors``, ``cerberus.schema``, ``cerberus.utils``,
``cerberus.validator`` and ``cerberus.platform`` are the very same module
objects as their ferberus counterparts.

As a pytest plugin (``-p ferberus.compat``) the alias is installed before
conftest files and tests are imported. ``FERBERUS_UPSTREAM_PACKAGE`` may point to the ``cerberus`` package
directory of an upstream checkout; its non-code subpackages (``tests``,
``benchmarks``) then become importable as ``cerberus.tests`` etc., which is
how the unmodified upstream suite is run against ferberus.
"""

import importlib.abc
import importlib.util
import os
import sys

_SHIM_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "_shim", "cerberus")


class _ShimFinder(importlib.abc.MetaPathFinder):
    def __init__(self, extra_search_locations):
        self.search_locations = [_SHIM_DIR] + list(extra_search_locations)

    def find_spec(self, fullname, path=None, target=None):
        if fullname != "cerberus":
            return None
        return importlib.util.spec_from_file_location(
            "cerberus",
            os.path.join(_SHIM_DIR, "__init__.py"),
            submodule_search_locations=list(self.search_locations),
        )


def installed():
    """Whether the ``cerberus`` alias is active in this interpreter."""
    return any(isinstance(x, _ShimFinder) for x in sys.meta_path)


def install(upstream_package=None):
    """
    Make ``import cerberus`` resolve to ferberus.

    :param upstream_package: Optional path to the ``cerberus`` package
        directory of an upstream checkout whose data-only subpackages
        (``tests``, ``benchmarks``) should be importable below ``cerberus``.
    """
    if installed():
        return
    existing = sys.modules.get("cerberus")
    if existing is not None:
        raise RuntimeError(
            "The real 'cerberus' package was already imported from %r; "
            "ferberus.compat.install() must be called before." % getattr(existing, "__file__", None)
        )
    if upstream_package is None:
        upstream_package = os.environ.get("FERBERUS_UPSTREAM_PACKAGE")
    extra = [upstream_package] if upstream_package else []
    sys.meta_path.insert(0, _ShimFinder(extra))
    import cerberus  # noqa: F401


def uninstall():
    """Remove the alias and every ``cerberus*`` entry from ``sys.modules``."""
    sys.meta_path[:] = [x for x in sys.meta_path if not isinstance(x, _ShimFinder)]
    for name in [x for x in sys.modules if x == "cerberus" or x.startswith("cerberus.")]:
        del sys.modules[name]


def pytest_addoption(parser):
    """Loaded as a pytest plugin (``-p ferberus.compat``): alias right away."""
    install()
