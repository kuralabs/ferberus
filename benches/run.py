#!/usr/bin/env python
"""
Benchmark harness: Cerberus vs ferberus on the workloads of ``workloads.py``.

    python benches/run.py                      # both implementations, all workloads
    python benches/run.py --impl ferberus -k flat --json out.json

Timing: each workload callable is calibrated to run for at least ``--min-time``
seconds per round; the best of ``--rounds`` rounds is reported (seconds per
call of the workload function). Results of both implementations are compared
for equality as a sanity check.
"""

import argparse
import gc
import importlib
import json
import os
import platform
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import workloads  # noqa: E402

CERBERUS_CHECKOUT = os.environ.get("CERBERUS_CHECKOUT", "/opt/cerberus")


def load(impl):
    if impl == "cerberus":
        if CERBERUS_CHECKOUT not in sys.path:
            sys.path.append(CERBERUS_CHECKOUT)
        lib = importlib.import_module("cerberus")
        assert "ferberus" not in (lib.__file__ or ""), lib.__file__
        return lib
    return importlib.import_module(impl)


def measure(fn, rounds, min_time):
    result = fn()
    number = 1
    while True:
        t0 = time.perf_counter()
        for _ in range(number):
            fn()
        elapsed = time.perf_counter() - t0
        if elapsed >= min_time or number >= 1 << 20:
            break
        number = max(number + 1, int(number * min(10.0, 1.2 * min_time / max(elapsed, 1e-9))))
    best = elapsed / number
    gc_was_enabled = gc.isenabled()
    gc.collect()
    gc.disable()
    try:
        for _ in range(rounds - 1):
            t0 = time.perf_counter()
            for _ in range(number):
                fn()
            best = min(best, (time.perf_counter() - t0) / number)
    finally:
        if gc_was_enabled:
            gc.enable()
    return best, number, result


def git_commit():
    try:
        return subprocess.check_output(
            ["git", "rev-parse", "--short", "HEAD"], stderr=subprocess.DEVNULL, text=True
        ).strip()
    except Exception:
        return "uncommitted"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--impl", action="append", choices=["cerberus", "ferberus"])
    parser.add_argument("-k", default="", help="substring filter for workload names")
    parser.add_argument("--rounds", type=int, default=7)
    parser.add_argument("--min-time", type=float, default=0.2)
    parser.add_argument("--json")
    args = parser.parse_args()
    impls = args.impl or ["cerberus", "ferberus"]

    names = [n for n in workloads.WORKLOADS if args.k in n]
    results = {}
    for name in names:
        row = {}
        checks = {}
        for impl in impls:
            lib = load(impl)
            fn = workloads.WORKLOADS[name](lib)
            best, number, check = measure(fn, args.rounds, args.min_time)
            row[impl] = best
            checks[impl] = check
        if len(set(map(repr, checks.values()))) != 1:
            raise SystemExit("result mismatch in %s: %r" % (name, checks))
        results[name] = row
        line = "%-30s" % name + "".join("  %s %10.3f ms" % (i, row[i] * 1e3) for i in impls)
        if len(impls) == 2:
            line += "  speedup %7.1fx" % (row[impls[0]] / row[impls[1]])
        print(line, flush=True)

    if args.json:
        meta = {
            "python": platform.python_version(),
            "implementation": sys.implementation.name,
            "free_threaded": not getattr(sys, "_is_gil_enabled", lambda: True)(),
            "machine": platform.machine(),
            "commit": git_commit(),
            "image": os.environ.get("FERBERUS_IMAGE_DIGEST", "unknown"),
            "rounds": args.rounds,
            "min_time": args.min_time,
        }
        os.makedirs(os.path.dirname(os.path.abspath(args.json)), exist_ok=True)
        with open(args.json, "w") as f:
            json.dump({"meta": meta, "results": results}, f, indent=2, sort_keys=True)


if __name__ == "__main__":
    main()
