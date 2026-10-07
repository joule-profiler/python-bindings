"""Profiles a command whose phases start at the tokens it prints."""

import sys

from joule_profiler import JouleProfiler

PROGRAM = """
import time
print("__LOAD__", flush=True)
numbers = [i * i for i in range(1_000_000)]
print("__SOLVE__", flush=True)
total = sum(n % 7 for n in numbers)
print("__REST__", flush=True)
time.sleep(0.2)
"""

if __name__ == "__main__":
    profiler = JouleProfiler(["rapl", "perf", "procfs"])
    run = profiler.profile([sys.executable, "-c", PROGRAM])

    for phase in run:
        print(f"\n{phase.name} ({phase.duration_ms} ms)")
        for source in phase.sources:
            for metric in source.metrics:
                print(f"  {source.name:<7} {metric.name:<22} {metric.value} {metric.unit}")

    print(f"\nexit code {run.summary.exit_code}, {run.summary.duration_ms} ms")
