from joule_profiler import JouleProfiler

if __name__ == "__main__":
    profiler = JouleProfiler(
        ["rapl", "perf", "procfs", "nvml"], define={"sources.nvml.ignore_on_failure": True}
    )

    for section, info in profiler.info().items():
        print(f"\n[{section}]")
        for key, value in info.items():
            if isinstance(value, list):
                value = ", ".join(value)
            print(f"  {key:<22} {value}")

    for source, metrics in profiler.list_sensors().items():
        print(f"\n{source} ({len(metrics)} metrics)")
        for name, unit in metrics.items():
            print(f"  {name:<30} {unit}")
