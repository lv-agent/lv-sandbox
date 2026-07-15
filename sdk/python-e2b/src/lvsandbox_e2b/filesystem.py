"""E2B-shaped Filesystem (cr-083 §6.2).

薄包装 lvsandbox _SessionFiles;E2B 路径("/foo")归一为 workspace 相对("foo")。
"""
from __future__ import annotations

import time
from typing import Callable, Optional

from ._handle import translating
from .models import FileInfo


def _rel(path: str) -> str:
    """E2B 绝对路径('/foo') -> workspace 相对('foo')。"""
    return path.lstrip("/")


def _basename(path: str) -> str:
    return path.rstrip("/").rsplit("/", 1)[-1]


class Filesystem:
    def __init__(self, client, sandbox_id: str):
        self._c = client
        self._sid = sandbox_id
        self._f = client.sessions.get(sandbox_id).files

    def read(self, path, *, format: str = "text", user: str = "user"):
        with translating("file"):
            data = self._f.get(_rel(path))
        return data.decode() if format == "text" else data

    def write(self, path, data, *, user: str = "user") -> FileInfo:
        if isinstance(data, str):
            data = data.encode()
        with translating("file"):
            self._f.put(_rel(path), data)
        # server PUT 只回 {ok};返回最小 FileInfo(避免额外 list 往返)。
        return FileInfo(name=_basename(path))

    def list(self, path, *, user: str = "user") -> list:
        with translating("file"):
            entries = self._f.list(_rel(path))
        return [FileInfo.from_file_entry(e) for e in entries]

    def remove(self, path, *, user: str = "user") -> None:
        with translating("file"):
            self._f.delete(_rel(path))

    def make_dir(self, path, *, user: str = "user") -> FileInfo:
        with translating("file"):
            self._f.make_dir(_rel(path))
        return FileInfo(name=_basename(path), is_dir=True)

    def exists(self, path, *, user: str = "user") -> bool:
        with translating("file"):
            return self._f.exists(_rel(path))

    def find(self, path, pattern, *, user: str = "user") -> list:
        with translating("file"):
            found = self._f.find(pattern, path=_rel(path))
        out = []
        for ff in found:
            fi = FileInfo.from_file_entry(ff.entry)
            fi.name = ff.path  # workspace 相对全路径作 name
            out.append(fi)
        return out

    def search(self, path, pattern, *, user: str = "user") -> list:
        with translating("file"):
            return self._f.search(pattern, path=_rel(path))

    def watch_dir(
        self,
        path,
        on_event: Callable,
        *,
        on_exit: Optional[Callable] = None,
        timeout: Optional[float] = None,
        user: str = "user",
    ):
        deadline = time.time() + timeout if timeout else None
        with translating("file"):
            for ev in self._c.sessions.get(self._sid).watch(
                _rel(path), timeout_secs=int(timeout or 60)
            ):
                on_event(ev)
                if deadline and time.time() > deadline:
                    break
        if on_exit:
            on_exit()
