#!/usr/bin/env python
"""
Make the cp39/cp310 musllinux wheels importable by every musl CPython.

    python scripts/musl-suffix.py dist/*.whl

Before 3.11, CPython used the glibc suffix ``.cpython-3X-<arch>-linux-gnu.so``
for extensions on musl as well, and maturin names the extension that way.
python-build-standalone (the interpreters uv installs) patches those versions
to ``linux-musl`` and then cannot import it. Every CPython also accepts a bare
``.so``, so the extension is renamed to ``_core.so`` and RECORD is rewritten.
Other wheels are left alone.
"""

import base64
import hashlib
import os
import re
import sys
import zipfile

WHEEL = re.compile(r"-cp3(9|10)-cp3\d+-musllinux_[^-]+\.whl$")
EXTENSION = re.compile(r"^(?P<stem>.+)\.cpython-3(9|10)-[^/]+-linux-gnu\.so$")


def record_line(name, data):
    digest = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode()
    return "%s,sha256=%s,%d" % (name, digest, len(data))


def fix(path):
    with zipfile.ZipFile(path) as wheel:
        members = [(info, wheel.read(info)) for info in wheel.infolist()]
    renames = {}
    for info, _ in members:
        match = EXTENSION.match(info.filename)
        if match:
            renames[info.filename] = match.group("stem") + ".so"
    if not renames:
        raise SystemExit("%s: no linux-gnu extension found" % path)

    record = next(
        info.filename for info, _ in members if info.filename.endswith(".dist-info/RECORD")
    )
    lines = []
    out = []
    for info, data in members:
        if info.filename == record:
            continue
        info.filename = renames.get(info.filename, info.filename)
        out.append((info, data))
        lines.append(record_line(info.filename, data))
    lines.append(record + ",,")

    tmp = path + ".tmp"
    with zipfile.ZipFile(tmp, "w", zipfile.ZIP_DEFLATED) as wheel:
        for info, data in out:
            wheel.writestr(info, data)
        wheel.writestr(record, "\n".join(lines) + "\n")
    os.replace(tmp, path)
    for old, new in renames.items():
        print("%s: %s -> %s" % (os.path.basename(path), old, new))


def main():
    for path in sys.argv[1:]:
        if WHEEL.search(os.path.basename(path)):
            fix(path)


if __name__ == "__main__":
    main()
