#!/usr/bin/env python
"""
Performance regression check for CI.

Compares the *speedup over Cerberus* of each workload (both measured in the
same run, on the same machine, which makes the number portable between
runners) against the recorded baseline and fails if one dropped below
``--tolerance`` times the baseline.

    python benches/check_regression.py current.json benches/results/baseline.json
"""

import argparse
import json
import sys


def speedups(path):
    with open(path) as f:
        results = json.load(f)["results"]
    return {name: row["cerberus"] / row["ferberus"] for name, row in results.items()}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("current")
    parser.add_argument("baseline")
    parser.add_argument("--tolerance", type=float, default=0.7)
    args = parser.parse_args()

    current, baseline = speedups(args.current), speedups(args.baseline)
    failed = False
    for name, expected in sorted(baseline.items()):
        if name not in current:
            print("MISSING  %-30s" % name)
            failed = True
            continue
        actual = current[name]
        ok = actual >= expected * args.tolerance
        failed |= not ok
        print(
            "%-8s %-30s %8.1fx (baseline %8.1fx)"
            % ("ok" if ok else "SLOWER", name, actual, expected)
        )
    for name in sorted(set(current) - set(baseline)):
        print("new      %-30s %8.1fx" % (name, current[name]))
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
