#!/usr/bin/env python
"""
Render benchmark results (``benches/run.py --json``) as a Markdown table.

    python benches/report.py benches/results/<file>.json [more.json ...]
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


def fmt(seconds):
    if seconds >= 1:
        return "%.2f s" % seconds
    if seconds >= 1e-3:
        return "%.2f ms" % (seconds * 1e3)
    return "%.1f µs" % (seconds * 1e6)


def main():
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
