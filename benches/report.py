#!/usr/bin/env python
"""
Render benchmark results (``benches/run.py --json``) as a Markdown table.

    python benches/report.py benches/results/<file>.json [more.json ...]
    python benches/report.py --readme benches/results/<file>.json

``--readme`` prints the short table of README.md: a few workloads, timed per
document (or per validator) instead of per batch.
"""

import json
import sys

DESCRIPTIONS = {
    "small_flat_valid": "1000 small flat documents, valid",
    "small_flat_invalid": "1000 small flat documents, 4 errors each",
    "small_flat_invalid_errors": "… plus rendering `Validator.errors`",
    "small_flat_no_normalize": "1000 small flat documents, `normalize=False`",
    "deep_nesting": "200 documents nested 8 levels deep",
    "large_list_of_integers": "one list of 5000 integers",
    "large_list_of_dicts": "one list of 1000 dicts",
    "one_huge_document": "one document: 2000 orders × items × attributes",
    "anyof_oneof": "500 documents against `anyof`/`oneof`/`noneof`",
    "normalization_heavy": "500 documents: coerce, default, rename, purge",
    "regex_heavy": "500 documents, 6 `regex` rules each",
    "schema_validation_cold": "`Validator(schema)`, 60 fields, caches cleared",
    "validator_construction_warm": "20 × `Validator(schema)`, 60 fields, schema known",
    "validator_construction_small": "100 × `Validator(schema)`, 8 fields, schema known",
    "custom_rules": "500 documents, subclass with custom rule, checks, coercer",
}

# README table: workload -> (description, number of units per measured call).
README = {
    "small_flat_valid": ("Validate a small document (8 fields)", 1000),
    "small_flat_invalid_errors": ("… invalid, 4 errors, reading `v.errors`", 1000),
    "deep_nesting": ("Validate a document nested 8 levels deep", 200),
    "normalization_heavy": ("Normalize a document (coerce, default, rename, purge)", 500),
    "large_list_of_dicts": ("Validate a list of 1000 dicts", 1),
    "custom_rules": ("Subclass with a custom rule, check and coercer", 500),
    "validator_construction_small": ("Create a `Validator` (8-field schema)", 100),
}


def fmt(seconds):
    if seconds >= 1:
        return "%.2f s" % seconds
    if seconds >= 1e-3:
        return "%.2f ms" % (seconds * 1e3)
    return "%.1f µs" % (seconds * 1e6)


def readme(path):
    with open(path) as f:
        data = json.load(f)
    print("| Operation | Cerberus | ferberus | Speedup |")
    print("| --- | ---: | ---: | ---: |")
    for name, (description, units) in README.items():
        row = data["results"][name]
        speedup = row["cerberus"] / row["ferberus"]
        print(
            "| %s | %s | %s | **%s×** |"
            % (
                description,
                fmt(row["cerberus"] / units),
                fmt(row["ferberus"] / units),
                ("%.0f" if speedup >= 10 else "%.1f") % speedup,
            )
        )


def main():
    if sys.argv[1:2] == ["--readme"]:
        readme(sys.argv[2])
        return
    for path in sys.argv[1:]:
        with open(path) as f:
            data = json.load(f)
        meta = data["meta"]
        print(
            "CPython %s%s, commit `%s`, image `%s`, best of %d rounds\n"
            % (
                meta["python"],
                " (free-threaded)" if meta.get("free_threaded") else "",
                meta["commit"],
                meta["image"][:19],
                meta["rounds"],
            )
        )
        print("| Workload | Cerberus | ferberus | Speedup |")
        print("| --- | ---: | ---: | ---: |")
        for name, row in data["results"].items():
            print(
                "| %s | %s | %s | **%.1f×** |"
                % (
                    DESCRIPTIONS.get(name, name),
                    fmt(row["cerberus"]),
                    fmt(row["ferberus"]),
                    row["cerberus"] / row["ferberus"],
                )
            )
        print()


if __name__ == "__main__":
    main()
