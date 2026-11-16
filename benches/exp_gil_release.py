#!/usr/bin/env python
"""
GIL-release experiment (DESIGN.md §7): from which subject length does it pay
to release the interpreter while the `regex` rule matches?

For each length: time per validation in one thread with and without
releasing, and the wall time 4 threads (one validator each) need for a fixed
amount of work.
"""

import sys
import threading
import time

import ferberus
from ferberus import _core

PATTERN = "(?:[a-z]+ )*[a-z]+"
NEVER = 1 << 60
THREADS = 4


def per_call(validator, document, seconds=0.3):
    count, start = 0, time.perf_counter()
    while time.perf_counter() - start < seconds:
        for _ in range(50):
            validator.validate(document)
        count += 50
    return (time.perf_counter() - start) / count


def wall_time(document, calls):
    """Seconds until THREADS threads have validated `calls` documents each."""
    barrier = threading.Barrier(THREADS + 1)

    def work():
        validator = ferberus.Validator({"s": {"regex": PATTERN}})
        validator.validate(document)
        barrier.wait()
        for _ in range(calls):
            validator.validate(document)

    workers = [threading.Thread(target=work) for _ in range(THREADS)]
    for worker in workers:
        worker.start()
    barrier.wait()
    start = time.perf_counter()
    for worker in workers:
        worker.join()
    return time.perf_counter() - start


gil = getattr(sys, "_is_gil_enabled", lambda: True)()
print("GIL enabled: %s" % gil)
print(
    "%8s | %-23s | %-27s | %s"
    % ("length", "1 thread, us per call", "%d threads, us per call" % THREADS, "gain")
)
print("%8s | %10s %12s | %12s %14s |" % ("", "held", "released", "held", "released"))
for length in (16, 64, 256, 1024, 2048, 4096, 16384, 65536):
    document = {"s": ("word " * (length // 5 + 1))[: length - 1] + "x"}
    validator = ferberus.Validator({"s": {"regex": PATTERN}})
    assert validator.validate(document)
    single, multi = [], []
    for threshold in (NEVER, 0):
        _core.set_regex_detach_threshold(threshold)
        single.append(per_call(validator, document))
        calls = max(200, int(0.4 / single[-1]))
        multi.append(min(wall_time(document, calls) for _ in range(3)) / (calls * THREADS))
    print(
        "%8d | %10.3f %12.3f | %12.3f %14.3f | %5.2fx"
        % (
            length,
            single[0] * 1e6,
            single[1] * 1e6,
            multi[0] * 1e6,
            multi[1] * 1e6,
            multi[0] / multi[1],
        )
    )
