"""E2B-shaped Sandbox (cr-083 §6.2).

薄翻译层:template->profile、E2B 异常映射、模型改名;底层走 lvsandbox.Client。
"""
from __future__ import annotations

import os
from typing import Any, Optional

from lvsandbox import Client

from ._handle import translating
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
