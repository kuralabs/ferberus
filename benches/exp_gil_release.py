#!/usr/bin/env python
"""
GIL-release experiment (DESIGN.md §7): from which subject length does it pay
to release the interpreter while the `regex` rule matches?

For each length: time per validation in one thread with and without
releasing, and the throughput of 4 threads (one validator each) relative to
one thread.
"""

import sys
import threading
import time

import ferberus
from ferberus import _core

PATTERN = "(?:[a-z]+ )*[a-z]+"
NEVER = 1 << 60


def per_call(validator, document, seconds=0.3):
    count, start = 0, time.perf_counter()
    while time.perf_counter() - start < seconds:
        for _ in range(50):
            validator.validate(document)
        count += 50
    return (time.perf_counter() - start) / count


def throughput(threads, document, seconds=0.5):
    counts = [0] * threads
    stop = threading.Event()

    def work(index):
        validator = ferberus.Validator({"s": {"regex": PATTERN}})
        while not stop.is_set():
            for _ in range(20):
                validator.validate(document)
            counts[index] += 20

    workers = [threading.Thread(target=work, args=(i,)) for i in range(threads)]
    for worker in workers:
        worker.start()
    time.sleep(seconds)
    stop.set()
    for worker in workers:
        worker.join()
    return sum(counts) / seconds


gil = getattr(sys, "_is_gil_enabled", lambda: True)()
print("GIL enabled: %s" % gil)
print(
    "%8s | %12s %12s | %s" % ("length", "held (us)", "released", "4 threads vs 1: held / released")
)
for length in (16, 64, 256, 1024, 4096, 16384, 65536):
    document = {"s": ("word " * (length // 5 + 1))[: length - 1] + "x"}
    validator = ferberus.Validator({"s": {"regex": PATTERN}})
    assert validator.validate(document)
    row = []
    for threshold in (NEVER, 0):
        _core.set_regex_detach_threshold(threshold)
        row.append(
            (per_call(validator, document), throughput(4, document) / throughput(1, document))
        )
    print(
        "%8d | %12.3f %12.3f | %5.2fx / %5.2fx"
        % (length, row[0][0] * 1e6, row[1][0] * 1e6, row[0][1], row[1][1])
    )
