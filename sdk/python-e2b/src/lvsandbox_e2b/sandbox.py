"""E2B-shaped Sandbox (cr-083 §6.2).

薄翻译层:template->profile、E2B 异常映射、模型改名;底层走 lvsandbox.Client。
"""
from __future__ import annotations

import os
from typing import Any, Optional

from lvsandbox import AsyncClient, Client

from ._handle import translating, translating_async
from ._mappings import template_to_profile
from .exceptions import SandboxException
from .models import SandboxInfo

_DEFAULT_URL = "http://127.0.0.1:8080"


def _resolve_base_url(base_url: Optional[str]) -> str:
    return base_url or os.environ.get("LVSANDBOX_URL", _DEFAULT_URL)


class Sandbox:
    """Synchronous E2B-shaped sandbox."""

    def __init__(
        self,
        *,
        sandbox_id: Optional[str] = None,
        base_url: Optional[str] = None,
        api_key: Optional[str] = None,
        timeout: float = 300.0,
        _client: Optional[Client] = None,
        _transport: Any = None,
    ):
        self._id = sandbox_id
        self._c = _client or Client(
            _resolve_base_url(base_url),
            api_key=api_key,
            timeout=timeout,
            transport=_transport,
        )

    @property
    def sandbox_id(self) -> str:
        if self._id is None:
            raise SandboxException("sandbox not created/connected")
        return self._id

    # ----- lifecycle -----

    @classmethod
    def create(
        cls,
        template: str = "base",
        *,
        alias: Optional[str] = None,
        timeout: Optional[int] = None,
        envs: Optional[dict] = None,
        metadata: Optional[dict] = None,
        api_key: Optional[str] = None,
        domain: Optional[str] = None,
        base_url: Optional[str] = None,
        _transport: Any = None,
    ) -> "Sandbox":
        # domain: E2B 签名对齐占位(lv-sandbox 用 base_url;domain 不接线)
        c = Client(_resolve_base_url(base_url), api_key=api_key, transport=_transport)
        kw: dict = {"profile": template_to_profile(template)}
        if timeout is not None:
            kw["timeout_secs"] = int(timeout)
        if alias is not None:
            kw["alias"] = alias
        if envs:
            kw["env"] = envs
        if metadata:
            kw["metadata"] = metadata
        with translating("session"):
            s = c.sessions.create(**kw)
        return cls(sandbox_id=s.id, _client=c)

    @classmethod
    def connect(
        cls,
        sandbox_id: str,
        *,
        api_key: Optional[str] = None,
        domain: Optional[str] = None,
        base_url: Optional[str] = None,
        _transport: Any = None,
    ) -> "Sandbox":
        # 不打 server;懒到首次使用。session-meta 跨重启 rebuild 由 server 负责。
        c = Client(_resolve_base_url(base_url), api_key=api_key, transport=_transport)
        return cls(sandbox_id=sandbox_id, _client=c)

    @classmethod
    def list(
        cls,
        *,
        query: Optional[dict] = None,
        state: Optional[str] = None,
        metadata: Optional[dict] = None,
        api_key: Optional[str] = None,
        domain: Optional[str] = None,
        base_url: Optional[str] = None,
        _transport: Any = None,
    ) -> list:
        # server 不支持过滤(design §2.3);客户端按 state/metadata(query) 过滤。
        c = Client(_resolve_base_url(base_url), api_key=api_key, transport=_transport)
        with translating("session"):
            infos = c.sessions.list()
        out = [SandboxInfo.from_session_info(i) for i in infos]
        if state:
            out = [s for s in out if s.state == state]
        want = metadata or query or {}
        if want:
            out = [
                s for s in out if all(s.metadata.get(k) == v for k, v in want.items())
            ]
        return out

    def kill(self) -> None:
        with translating("session"):
            self._c.sessions.destroy(self.sandbox_id)

    def get_info(self) -> SandboxInfo:
        with translating("session"):
            i = self._c.sessions.get(self.sandbox_id).info()
        return SandboxInfo.from_session_info(i)

    def set_timeout(self, timeout: int) -> None:
        with translating("session"):
            self._c.sessions.get(self.sandbox_id).set_timeout(int(timeout))

    def set_metadata(self, metadata: dict) -> None:
        with translating("session"):
            self._c.sessions.get(self.sandbox_id).set_metadata(metadata)

    def replicate(self, n: int = 1, template: Optional[str] = None) -> list:
        """cr-083 §4: N× from_snapshot(shim 串行放大,无真 fork 语义)。"""
        with translating("session"):
            snap = self._c.sessions.get(self.sandbox_id).snapshot()
            out = []
            for _ in range(n):
                s = self._c.sessions.create(
                    profile=template_to_profile(template or "base"),
                    from_snapshot=snap,
                )
                out.append(Sandbox(sandbox_id=s.id, _client=self._c))
        return out

    # ----- sub-accessors (lazy import to avoid cycles) -----

    @property
    def commands(self):
        from .commands import Commands

        return Commands(self._c, self.sandbox_id)

    @property
    def filesystem(self):
        from .filesystem import Filesystem

        return Filesystem(self._c, self.sandbox_id)

    @property
    def snapshot(self):
        from .snapshot import Snapshot

        return Snapshot(self._c, self.sandbox_id)


class AsyncSandbox:
    """Async E2B-shaped sandbox (cr-083 §6.2 AsyncSandbox)。

    镜像 :class:`Sandbox`,底层走 lvsandbox.AsyncClient;全 async。
    """

    def __init__(
        self,
        *,
        sandbox_id: Optional[str] = None,
        base_url: Optional[str] = None,
        api_key: Optional[str] = None,
        timeout: float = 300.0,
        _client: Optional[AsyncClient] = None,
        _transport: Any = None,
    ):
        self._id = sandbox_id
        self._c = _client or AsyncClient(
            _resolve_base_url(base_url),
            api_key=api_key,
            timeout=timeout,
            transport=_transport,
        )

    @property
    def sandbox_id(self) -> str:
        if self._id is None:
            raise SandboxException("sandbox not created/connected")
        return self._id

    @classmethod
    async def create(
        cls,
        template: str = "base",
        *,
        alias: Optional[str] = None,
        timeout: Optional[int] = None,
        envs: Optional[dict] = None,
        metadata: Optional[dict] = None,
        api_key: Optional[str] = None,
        domain: Optional[str] = None,
        base_url: Optional[str] = None,
        _transport: Any = None,
    ) -> "AsyncSandbox":
        c = AsyncClient(_resolve_base_url(base_url), api_key=api_key, transport=_transport)
        kw: dict = {"profile": template_to_profile(template)}
        if timeout is not None:
            kw["timeout_secs"] = int(timeout)
        if alias is not None:
            kw["alias"] = alias
        if envs:
            kw["env"] = envs
        if metadata:
            kw["metadata"] = metadata
        async with translating_async("session"):
            s = await c.sessions.create(**kw)
        return cls(sandbox_id=s.id, _client=c)

    @classmethod
    async def connect(
        cls,
        sandbox_id: str,
        *,
        api_key: Optional[str] = None,
        domain: Optional[str] = None,
        base_url: Optional[str] = None,
        _transport: Any = None,
    ) -> "AsyncSandbox":
        c = AsyncClient(_resolve_base_url(base_url), api_key=api_key, transport=_transport)
        return cls(sandbox_id=sandbox_id, _client=c)

    @classmethod
    async def list(
        cls,
        *,
        query: Optional[dict] = None,
        state: Optional[str] = None,
        metadata: Optional[dict] = None,
        api_key: Optional[str] = None,
        domain: Optional[str] = None,
        base_url: Optional[str] = None,
        _transport: Any = None,
    ) -> list:
        c = AsyncClient(_resolve_base_url(base_url), api_key=api_key, transport=_transport)
        async with translating_async("session"):
            infos = await c.sessions.list()
        out = [SandboxInfo.from_session_info(i) for i in infos]
        if state:
            out = [s for s in out if s.state == state]
        want = metadata or query or {}
        if want:
            out = [
                s for s in out if all(s.metadata.get(k) == v for k, v in want.items())
            ]
        return out

    async def kill(self) -> None:
        async with translating_async("session"):
            await self._c.sessions.destroy(self.sandbox_id)

    async def get_info(self) -> SandboxInfo:
        async with translating_async("session"):
            s = await self._c.sessions.get(self.sandbox_id)
            i = await s.info()
        return SandboxInfo.from_session_info(i)

    async def set_timeout(self, timeout: int) -> None:
        async with translating_async("session"):
            s = await self._c.sessions.get(self.sandbox_id)
            await s.set_timeout(int(timeout))

    async def set_metadata(self, metadata: dict) -> None:
        async with translating_async("session"):
            s = await self._c.sessions.get(self.sandbox_id)
            await s.set_metadata(metadata)

    @property
    def commands(self):
        from .commands import AsyncCommands

        return AsyncCommands(self._c, self.sandbox_id)

    @property
    def filesystem(self):
        from .filesystem import AsyncFilesystem

        return AsyncFilesystem(self._c, self.sandbox_id)

    @property
    def snapshot(self):
        from .snapshot import AsyncSnapshot

        return AsyncSnapshot(self._c, self.sandbox_id)
