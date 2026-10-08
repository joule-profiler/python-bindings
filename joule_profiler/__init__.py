from __future__ import annotations

import json
import os
import shlex
import sys
import threading
from collections.abc import Iterable, Iterator, Mapping, Sequence
from contextlib import ContextDecorator
from types import TracebackType
from typing import Any, Callable, TypeVar, cast
from functools import wraps

from ._core import Metric, Phase, Run, SourceMetrics, Summary, _Profiler, _Session

__all__ = [
    "JouleProfiler",
    "Metric",
    "Phase",
    "Run",
    "Session",
    "SourceMetrics",
    "Summary",
    "phase",
]

_lock = threading.Lock()
_active: Session | None = None

_SERVE = "import sys; sys.path.insert(0, {root!r}); from joule_profiler._core import _serve; _serve()"


def _profiler_command() -> list[str]:
    if not sys.executable:
        raise RuntimeError("the Python interpreter is unknown, so no profiler can be started")
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    return [sys.executable, "-I", "-S", "-c", _SERVE.format(root=root)]


class JouleProfiler:
    def __init__(
        self,
        sources: str | Iterable[str] = (),
        *,
        config: str | os.PathLike[str] | None = None,
        define: Mapping[str, object] | None = None,
        defer: bool | None = None,
    ) -> None:
        names = [sources] if isinstance(sources, str) else list(sources)
        values = list(define.items()) if define else []
        if defer is not None:
            values.append(("profiler.defer", defer))
        encoded = json.dumps(values, default=os.fspath)
        self._profiler = _Profiler(names, config, encoded)

    def list_sensors(self) -> dict[str, dict[str, str]]:
        return cast("dict[str, dict[str, str]]", json.loads(self._profiler.list_sensors()))

    def info(self) -> dict[str, dict[str, object]]:
        return cast("dict[str, dict[str, object]]", json.loads(self._profiler.info()))

    def profile(
        self,
        command: str | Sequence[str],
        *,
        token_pattern: str | None = None,
        stdout_file: str | os.PathLike[str] | None = None,
        use_root: bool | None = None,
    ) -> Run:
        """A phase starts at each output line matching ``token_pattern``: flush the tokens."""
        arguments = shlex.split(command) if isinstance(command, str) else list(command)
        return self._profiler.profile(arguments, token_pattern, stdout_file, use_root)

    def session(self) -> Session:
        return Session(self._profiler.session(_profiler_command()))

class Session(ContextDecorator):
    def __init__(self, inner: _Session) -> None:
        self._inner = inner
        self._over = False
        self._owner = os.getpid()

    def __enter__(self) -> Session:
        global _active
        with _lock:
            if _active is not None:
                raise RuntimeError("this program is already being measured")
            _active = self
        return self


    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc_value: BaseException | None,
        traceback: TracebackType | None,
    ) -> None:
        global _active
        exit_code = 0 if exc_type is None else 1
        with _lock:
            _active = None
        if self._owner == os.getpid():
            self._finish(exit_code)

    def phase(self, name: str) -> None:
        if os.getpid() != self._owner:
            raise RuntimeError(
                "a session only takes phases from the process that started it"
            )
        self._inner.phase(name)

    @property
    def phases(self) -> Iterator[Phase]:
        return iter(self._inner.run)

    def poll(self) -> Phase | None:
        """A phase that already ended, or ``None`` without waiting."""
        return self._inner.run.poll()

    @property
    def summary(self) -> Summary:
        if not self._over:
            raise RuntimeError("the session is still running")
        return self._inner.run.summary

    def _finish(self, exit_code: int) -> None:
        self._over = True
        self._inner.finish(exit_code)

_Function = TypeVar("_Function", bound=Callable[..., Any])

def phase(name: str) -> Callable[[_Function], _Function]:
    def start() -> None:
        session = _active
        if session is not None and session._owner == os.getpid():
            session.phase(name)

    def decorator(func: _Function) -> _Function:
        @wraps(func)
        def wrapper(*args: Any, **kwargs: Any) -> Any:
            start()
            return func(*args, **kwargs)

        return cast(_Function, wrapper)

    start()
    return decorator