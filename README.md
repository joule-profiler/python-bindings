# joule-profiler for Python

Python bindings for [joule-profiler](https://github.com/joule-profiler/joule-profiler), written in Rust with PyO3. Measure the energy and resources a program uses, phase by phase, with the sources implemented by joule-profiler: RAPL, perf events, procfs, cgroups, NVML and AMD SMI.

There are two ways to profile:

- `JouleProfiler.profile` runs a command, and starts a new phase each time the command prints a token.
- `JouleProfiler.session` measures the running Python program itself. A new phase starts at each `session.phase(...)` or `joule_profiler.phase(...)`.

## Links

- Joule Profiler: [github.com/joule-profiler/joule-profiler](https://github.com/joule-profiler/joule-profiler)
- Documentation: [joule-profiler.github.io](https://joule-profiler.github.io), including the [sources and their settings](https://joule-profiler.github.io/sources/overview.html)

## Build, test, run

### Build

```bash
uv venv .venv && source .venv/bin/activate
uv pip install maturin pytest mypy
maturin develop --release
```

### Test

```bash
pytest
mypy --strict joule_profiler tests
cargo test
```

### Run

```bash
python examples/command.py
python examples/session.py
```

## Examples

The `examples/` directory has one script for each way to profile.

### Profiling a command: `examples/command.py`

```python
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
```

### Profiling the already running program itself: `examples/session.py`

```python
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
        numbers = load()
        solve(numbers)
        session.phase("rest")
        time.sleep(0.2)

    for phase in session:
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
```
