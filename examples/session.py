import time
import joule_profiler
from joule_profiler import JouleProfiler

@joule_profiler.phase("load")
def load() -> list[int]:
    return [i * i for i in range(1_000_000)]

def solve(numbers: list[int]) -> int:
    joule_profiler.phase("solve")
    return sum(n % 7 for n in numbers)

if __name__ == "__main__":
    profiler = JouleProfiler(
        ["rapl", "perf", "procfs", "nvml"], define={"sources.procfs.poll_interval": "100ms", "sources.nvml.ignore_on_failure": True}
    )

    with profiler.session() as session:
        session.poll() # return None
        numbers = load()
        # session.poll() # would return the first phase
        solve(numbers)
        session.phase("rest")
        time.sleep(0.2)

    for phase in session.phases:
        metrics = {
            f"{metric.name}": f"{metric.value} {metric.unit}"
            for source in phase.sources
            for metric in source.metrics
        }

        print(f"{phase.name:<16} {phase.duration_ms:6} ms")
        print("{")
        for name, value in metrics.items():
            print(f"    {name!r}: {value!r},")
        print("}\n")

    print(session.summary)
