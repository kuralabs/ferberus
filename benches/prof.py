#!/usr/bin/env python
"""
Profile one workload of ``workloads.py``.

    # cProfile, top functions by own time
    python benches/prof.py cerberus small_flat_valid --cprofile

    # endless-ish loop for sampling profilers
    py-spy record -o profiles/x.svg -- python benches/prof.py cerberus small_flat_valid
    perf record -g -- python benches/prof.py ferberus small_flat_valid
"""

import argparse
import cProfile
import pstats
import time

import workloads
from run import load


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("impl", choices=["cerberus", "ferberus"])
    parser.add_argument("workload", choices=sorted(workloads.WORKLOADS))
    parser.add_argument("--seconds", type=float, default=5.0)
    parser.add_argument("--cprofile", action="store_true")
    parser.add_argument("--top", type=int, default=18)
    args = parser.parse_args()

    fn = workloads.WORKLOADS[args.workload](load(args.impl))
    fn()

    if args.cprofile:
        profiler = cProfile.Profile()
        profiler.enable()
        t0 = time.perf_counter()
        while time.perf_counter() - t0 < args.seconds:
            fn()
        profiler.disable()
        stats = pstats.Stats(profiler)
        stats.strip_dirs().sort_stats("tottime").print_stats(args.top)
    else:
        t0 = time.perf_counter()
        while time.perf_counter() - t0 < args.seconds:
            fn()


if __name__ == "__main__":
    main()
