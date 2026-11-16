"""Print outcome distribution of generated scenarios (generator quality check).

python -m tests.differential.stats [examples] [validators,comma,separated]
"""

import collections
import sys

from hypothesis import HealthCheck, given, settings

sys.path.append("/opt/cerberus")
import cerberus  # noqa: E402

from . import strategies as S  # noqa: E402
from .harness import run  # noqa: E402

n = int(sys.argv[1]) if len(sys.argv) > 1 else 500
validators = tuple(sys.argv[2].split(",")) if len(sys.argv) > 2 else ("plain",)
stats = collections.Counter()


@settings(max_examples=n, deadline=None, database=None, suppress_health_check=list(HealthCheck))
@given(S.scenarios(validators=validators))
def collect(scenario):
    records = run(cerberus, scenario)
    construct = records[1][1]
    stats["construct " + (construct[0] if construct[0] == "ok" else construct[1])] += 1
    for record in records[2:]:
        outcome = record[1]
        if outcome[0] == "ok":
            stats["%s -> %s" % (record[0], str(outcome[1])[:24])] += 1
            nerr = record[3]["_errors"]
            if nerr[0] == "ok":
                stats["  errors: %s" % min(len(nerr[1][1]), 4)] += 1
        else:
            stats["%s raised %s" % (record[0], outcome[1])] += 1


collect()
for key, count in sorted(stats.items()):
    print("%6d  %s" % (count, key))
