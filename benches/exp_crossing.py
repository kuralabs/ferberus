#!/usr/bin/env python
"""
Crossing-cost experiment (DESIGN.md): convert documents into Rust structures
vs. reading the Python objects in place.

Requires a build with the ``experiments`` feature:
    VIRTUAL_ENV=/opt/venvs/3.14 maturin develop --release --features experiments
"""

import timeit

import ferberus._core as core
import workloads

flat = workloads.flat_documents(1)[0]
items = {"items": workloads.item_documents(1000)}
ints = {"values": list(range(5000))}
nested = workloads.nested_document(8)

for name, doc in (
    ("flat (8 fields)", flat),
    ("nested (8 levels)", nested),
    ("1000 item dicts", items),
    ("5000 ints", ints),
):
    assert core.exp_convert_walk(doc) == core.exp_direct_walk(doc)
    row = []
    for fn in (core.exp_direct_walk, core.exp_convert_only, core.exp_convert_walk):
        number = max(1, int(2e5 // (len(str(doc)) + 1)) * 20)
        best = min(timeit.repeat(lambda: fn(doc), number=number, repeat=7)) / number
        row.append(best * 1e6)
    print(
        "%-20s direct %9.3f us | convert %9.3f us | convert+walk %9.3f us | ratio %4.1fx"
        % (name, row[0], row[1], row[2], row[2] / row[0])
    )
