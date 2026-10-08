from __future__ import annotations

import json
import multiprocessing
import os
import sys
import threading
import time
from collections.abc import Iterable
from pathlib import Path

import pytest

import joule_profiler
from joule_profiler import JouleProfiler, Phase

TWO_TOKENS = "print('__A__', flush=True); print('__B__', flush=True)"


def names(phases: Iterable[Phase]) -> list[str]:
    return [phase.name for phase in phases]


def test_a_command_is_measured_phase_by_phase() -> None:
    run = JouleProfiler("procfs").profile([sys.executable, "-c", TWO_TOKENS], stdout_file=os.devnull)

    phases = list(run)

    assert names(phases) == ["START -> __A__", "__A__ -> __B__", "__B__ -> END"]
    assert run.summary.exit_code == 0
    assert run.summary.phases == 3
    assert phases[1].start_line == 1
    source = phases[1].sources[0]
    assert source.name == "procfs"
    assert all(isinstance(metric.value, int) for metric in source.metrics)


def test_a_command_can_be_a_string_and_its_output_a_file(tmp_path: Path) -> None:
    output = tmp_path / "out.txt"
    command = f"{sys.executable} -c \"print('__X__')\""

    run = JouleProfiler("procfs").profile(command, stdout_file=output)

    assert names(run) == ["START -> __X__", "__X__ -> END"]
    assert output.read_text() == "__X__\n"


def test_the_summary_waits_for_the_end_of_the_command_and_keeps_its_phases() -> None:
    run = JouleProfiler("procfs").profile(
        [sys.executable, "-c", "print('__A__'); raise SystemExit(3)"], stdout_file=os.devnull
    )

    assert run.summary.exit_code == 3
    assert names(run) == ["START -> __A__", "__A__ -> END"]


def test_a_program_measures_itself() -> None:
    profiler = JouleProfiler("procfs", define={"sources.procfs.poll_interval": "5ms"})

    with profiler.session() as session:
        session.phase("load")
        sum(range(100_000))
        joule_profiler.phase("solve")

    assert names(session.phases) == ["START -> load", "load -> solve", "solve -> END"]
    assert session.summary.exit_code == 0


def test_a_deferred_session_measures_the_same_phases() -> None:
    with JouleProfiler("procfs", defer=True).session() as session:
        session.phase("only")

    assert names(session.phases) == ["START -> only", "only -> END"]


def test_the_phases_of_a_session_come_as_they_end() -> None:
    seen: list[str] = []

    def read(session: joule_profiler.Session) -> None:
        for phase in session.phases:
            seen.append(phase.name)

    with JouleProfiler("procfs").session() as session:
        reader = threading.Thread(target=read, args=(session,))
        reader.start()
        session.phase("a")
        session.phase("b")
        deadline = time.monotonic() + 5
        while len(seen) < 2 and time.monotonic() < deadline:
            time.sleep(0.01)
        assert seen == ["START -> a", "a -> b"], "the phases came before the end of the session"

    reader.join(timeout=5)
    assert seen == ["START -> a", "a -> b", "b -> END"]


def test_a_session_gives_the_phases_already_ended_without_waiting() -> None:
    with JouleProfiler("procfs").session() as session:
        assert session.poll() is None, "no phase has ended yet"
        session.phase("a")
        deadline = time.monotonic() + 5
        polled = session.poll()
        while polled is None and time.monotonic() < deadline:
            time.sleep(0.01)
            polled = session.poll()
        assert polled is not None and polled.name == "START -> a"
        assert session.poll() is None, "the phase a has not ended yet"

    assert names(iter(session.poll, None)) == ["a -> END"]


def test_a_block_that_raises_still_ends_its_session() -> None:
    profiler = JouleProfiler("procfs")

    with pytest.raises(ValueError), profiler.session() as session:
        session.phase("broken")
        raise ValueError

    assert session.summary.exit_code == 1
    assert names(session.phases) == ["START -> broken", "broken -> END"]


def test_a_program_is_measured_by_one_session_at_a_time() -> None:
    profiler = JouleProfiler("procfs")

    with profiler.session(), pytest.raises(RuntimeError, match="already"):
        with profiler.session():
            pass


def test_the_summary_waits_for_the_end_of_the_session() -> None:
    with JouleProfiler("procfs").session() as session:
        with pytest.raises(RuntimeError, match="still running"):
            _ = session.summary


def test_a_phase_outside_of_a_session_does_nothing() -> None:
    joule_profiler.phase("nothing")


def test_a_session_that_cannot_measure_says_why() -> None:
    profiler = JouleProfiler(
        "cgroup", define={"sources.cgroup.root": "/nonexistent", "sources.cgroup.name": "run"}
    )

    with pytest.raises(RuntimeError, match="/nonexistent"), profiler.session():
        pass


def test_an_unknown_source_is_refused() -> None:
    with pytest.raises(ValueError, match="unknown source `nope`"):
        JouleProfiler(["procfs", "nope"])
    with pytest.raises(ValueError, match="unknown source `rpl`"):
        JouleProfiler(define={"sources.rpl.backend": "powercap"})


def test_a_misspelled_key_is_refused() -> None:
    with pytest.raises(ValueError, match="defr"):
        JouleProfiler("procfs", define={"profiler.defr": True})
    with pytest.raises(ValueError, match="`profiler..defer`"):
        JouleProfiler("procfs", define={"profiler..defer": True})
    with pytest.raises(ValueError, match="dicts"):
        JouleProfiler("procfs", define={"injector.stdout_file": None})
    with pytest.raises(ValueError, match="exporter"):
        JouleProfiler("procfs", define={"exporter.format": "json"})


def test_a_table_that_does_not_describe_its_source_is_reported() -> None:
    with pytest.raises(RuntimeError, match="globl"):
        JouleProfiler(define={"sources.procfs.globl": True}).list_sensors()


def test_the_configuration_is_read_as_the_command_line_reads_it(tmp_path: Path) -> None:
    config = tmp_path / "joule-profiler.toml"
    config.write_text('[sources.procfs]\nglobal = false\n[injector]\ntoken_pattern = "never"\n')
    profiler = JouleProfiler(
        config=config,
        define={"sources.procfs.global": True, "injector.token_pattern": "STEP [a-z]+"},
    )
    command = [sys.executable, "-c", "print('STEP one', flush=True)"]

    phases = list(profiler.profile(command, stdout_file=os.devnull))

    assert names(phases) == ["START -> STEP one", "STEP one -> END"]
    [source] = phases[0].sources
    assert source.name == "procfs"
    assert any(
        metric.name == "global_mem_used_max" and metric.unit == "B" for metric in source.metrics
    )

    run = profiler.profile(command, token_pattern="nothing", stdout_file=os.devnull)
    assert names(run) == ["START -> END"]


def test_the_machine_and_the_sources_are_described() -> None:
    info = JouleProfiler("procfs").info()

    assert list(info) == ["machine", "procfs"]
    assert info["procfs"]["global"] is False
    assert info["procfs"]["poll_interval"] == "10ms"


def test_the_sensors_are_listed() -> None:
    sensors = JouleProfiler("procfs").list_sensors()

    assert list(sensors) == ["procfs"]
    assert sensors["procfs"]["proc_rss_max"] == "B"


def _phase_from_a_child(queue: multiprocessing.Queue[str]) -> None:
    joule_profiler.phase("ignored")
    try:
        joule_profiler._active.phase("refused")  # type: ignore[union-attr]
    except RuntimeError as error:
        queue.put(str(error))


def _measure_in_a_child(queue: multiprocessing.Queue[list[str]]) -> None:
    with JouleProfiler("procfs").session() as session:
        session.phase("child")
    queue.put(names(session.phases))


def test_a_forked_child_measures_itself() -> None:
    context = multiprocessing.get_context("fork")
    queue: multiprocessing.Queue[list[str]] = context.Queue()

    child = context.Process(target=_measure_in_a_child, args=(queue,))
    child.start()
    child.join()

    assert queue.get(timeout=5) == ["START -> child", "child -> END"]


def test_a_forked_child_cannot_take_phases_of_the_session() -> None:
    context = multiprocessing.get_context("fork")
    queue: multiprocessing.Queue[str] = context.Queue()

    with JouleProfiler("procfs").session() as session:
        child = context.Process(target=_phase_from_a_child, args=(queue,))
        child.start()
        child.join()
        session.phase("mine")

    assert "process that started it" in queue.get(timeout=5)
    assert names(session.phases) == ["START -> mine", "mine -> END"]
