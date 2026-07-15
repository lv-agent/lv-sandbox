"""HTTP client for lv-sandbox.

Mirrors the v0.3 HTTP API: one-shot jobs, persistent sessions (with files,
snapshots, volumes), streaming stdout (SSE), and worker introspection.
"""
from __future__ import annotations

import time
from typing import Any, Iterator, Optional

import httpx

from .errors import LvApiError
from .models import FileEntry, FoundFile, JobResult, SearchHit, SessionInfo, StreamEvent
from .sse import iter_sse, iter_sse_async


def _raise_for_status(resp: httpx.Response) -> None:
    if resp.status_code >= 400:
        try:
            msg = resp.json().get("error", resp.text)
        except Exception:
            msg = resp.text
        raise LvApiError(resp.status_code, msg)


class _Base:
    def __init__(self, client: "Client"):
        self._c = client


class Jobs(_Base):
    """One-shot jobs (``POST /jobs`` + poll)."""

    @staticmethod
    def _body(argv, profile, timeout, env, stdin, job_id) -> dict:
        body: dict = {"argv": list(argv), "profile_name": profile}
        if timeout is not None:
            body["timeout"] = timeout
        if env:
            body["custom_env"] = dict(env)
        if stdin is not None:
            body["stdin"] = stdin
        if job_id is not None:
            body["job_id"] = job_id
        return body

    def run(
        self,
        argv,
        *,
        profile: str = "shell",
        timeout: Optional[str] = None,
        env: Optional[dict] = None,
        stdin: Optional[str] = None,
        job_id: Optional[str] = None,
        poll_interval: float = 0.1,
        poll_timeout: float = 300.0,
    ) -> JobResult:
        """Submit a job and poll until it reaches a terminal state."""
        body = self._body(argv, profile, timeout, env, stdin, job_id or f"job-{int(time.time() * 1000)}")
        resp = self._c._post("/api/v1/jobs", json=body)
        jid = resp["job_id"]
        deadline = time.time() + poll_timeout
        while True:
            r = self._c._get(f"/api/v1/jobs/{jid}")
            if r.get("status") != "Running":
                return JobResult.from_json(r)
            if time.time() > deadline:
                raise LvApiError(408, f"job {jid} polling timed out")
            time.sleep(poll_interval)

    def stream(
        self,
        argv,
        *,
        profile: str = "shell",
        timeout: Optional[str] = None,
        env: Optional[dict] = None,
        stdin: Optional[str] = None,
        job_id: Optional[str] = None,
    ) -> Iterator[StreamEvent]:
        """Submit a job with ``?stream=true`` and yield SSE events."""
        body = self._body(argv, profile, timeout, env, stdin, job_id or f"job-{int(time.time() * 1000)}")
        with self._c._http.stream(
            "POST", "/api/v1/jobs?stream=true", json=body, timeout=self._c._timeout
        ) as resp:
            _raise_for_status(resp)
            yield from iter_sse(resp)

    def get(self, job_id: str) -> JobResult:
        return JobResult.from_json(self._c._get(f"/api/v1/jobs/{job_id}"))

    def cancel(self, job_id: str) -> dict:
        return self._c._post(f"/api/v1/jobs/{job_id}/cancel")


class _SessionFiles(_Base):
    def __init__(self, client: "Client", sid: str):
        super().__init__(client)
        self._sid = sid

    def put(self, path: str, data: bytes) -> dict:
        return self._c._put(f"/api/v1/sessions/{self._sid}/files/{path}", content=data)

    def get(self, path: str) -> bytes:
        return self._c._get_raw(f"/api/v1/sessions/{self._sid}/files/{path}")

    def list(self, path: str = "") -> list[FileEntry]:
        r = self._c._get(
            f"/api/v1/sessions/{self._sid}/files", params={"path": path} if path else {}
        )
        return [FileEntry.from_json(e) for e in r.get("entries", [])]

    def delete(self, path: str) -> dict:
        return self._c._delete(f"/api/v1/sessions/{self._sid}/files/{path}")

    def make_dir(self, path: str) -> dict:
        """cr-083 P1c: POST mkdir(workspace 相对 path)。"""
        return self._c._post(
            f"/api/v1/sessions/{self._sid}/mkdir", json={"path": path}
        )

    def exists(self, path: str) -> bool:
        """cr-083 P1c: HEAD exists(200=True, 404=False, 其余≥400 抛)。裸状态,无 body。"""
        resp = self._c._http.head(f"/api/v1/sessions/{self._sid}/files/{path}")
        if resp.status_code == 404:
            return False
        _raise_for_status(resp)
        return resp.status_code < 400

    def find(self, pattern: str, *, path: str = "", limit: Optional[int] = None):
        """cr-083 P1c: glob find。返回 list[FoundFile](path+entry)。truncated 暂不返回。"""
        body: dict = {"pattern": pattern}
        if path:
            body["path"] = path
        if limit is not None:
            body["limit"] = limit
        r = self._c._post(f"/api/v1/sessions/{self._sid}/files/find", json=body)
        return [FoundFile.from_json(f) for f in r.get("files", [])]

    def search(self, pattern: str, *, path: str = ""):
        """cr-083 P1c: regex 内容搜索。返回 list[SearchHit](grep 式 line/text)。"""
        body: dict = {"pattern": pattern}
        if path:
            body["path"] = path
        r = self._c._post(f"/api/v1/sessions/{self._sid}/files/search", json=body)
        return [SearchHit.from_json(h) for h in r.get("results", [])]


class Session:
    """A persistent sandbox session."""

    def __init__(self, client: "Client", session_id: str):
        self._c = client
        self.id = session_id
        self.files = _SessionFiles(client, session_id)

    def info(self) -> SessionInfo:
        return SessionInfo.from_json(self._c._get(f"/api/v1/sessions/{self.id}"))

    def exec(
        self,
        argv,
        *,
        timeout: Optional[str] = None,
        env: Optional[dict] = None,
        stdin: Optional[str] = None,
        cwd: Optional[str] = None,
    ) -> JobResult:
        """Run a command in this session's persistent workspace (synchronous)."""
        # cr-083 P1a: cwd(相对 workspace)。env 字段为 custom_env(server 契约)。
        body: dict = {"argv": list(argv)}
        if timeout is not None:
            body["timeout"] = timeout
        if env:
            body["custom_env"] = dict(env)
        if stdin is not None:
            body["stdin"] = stdin
        if cwd is not None:
            body["cwd"] = cwd
        return JobResult.from_json(
            self._c._post(f"/api/v1/sessions/{self.id}/exec", json=body)
        )

    def exec_stream(
        self,
        argv,
        *,
        timeout: Optional[str] = None,
        env: Optional[dict] = None,
        stdin: Optional[str] = None,
    ) -> Iterator[StreamEvent]:
        """Stream a session exec over SSE."""
        body: dict = {"argv": list(argv)}
        if timeout is not None:
            body["timeout"] = timeout
        if env:
            body["custom_env"] = dict(env)
        if stdin is not None:
            body["stdin"] = stdin
        with self._c._http.stream(
            "POST",
            f"/api/v1/sessions/{self.id}/exec?stream=true",
            json=body,
            timeout=self._c._timeout,
        ) as resp:
            _raise_for_status(resp)
            yield from iter_sse(resp)

    def set_timeout(self, timeout_secs: int) -> dict:
        """cr-083: PATCH session timeout(seconds)."""
        return self._c._patch(
            f"/api/v1/sessions/{self.id}", json={"timeout_secs": timeout_secs}
        )

    def set_metadata(self, metadata: dict) -> dict:
        """cr-083: PATCH session metadata(full replace, server semantics)."""
        return self._c._patch(
            f"/api/v1/sessions/{self.id}", json={"metadata": dict(metadata)}
        )

    def set_alias(self, alias: str) -> dict:
        """cr-083: PATCH session alias."""
        return self._c._patch(f"/api/v1/sessions/{self.id}", json={"alias": alias})

    def watch(self, path: str = "", *, timeout_secs: int = 60):
        """cr-083 P1b: watch a workspace subtree via SSE.

        Yields dicts ``{"event": "created"|"modified"|"removed", "paths": [...]}``.
        notify Access/Any/Other kinds are dropped server-side(降噪)。
        """
        url = f"/api/v1/sessions/{self.id}/files/watch"
        params: dict = {"timeout_secs": timeout_secs}
        if path:
            params["path"] = path
        with self._c._http.stream(
            "GET", url, params=params, timeout=self._c._timeout
        ) as resp:
            _raise_for_status(resp)
            for ev in iter_sse(resp):
                paths = ev.data.get("paths") if isinstance(ev.data, dict) else []
                yield {"event": ev.type, "paths": paths or []}

    def snapshot(self) -> str:
        return self._c._post(f"/api/v1/sessions/{self.id}/snapshot")["snapshot_id"]

    def destroy(self) -> dict:
        return self._c._delete(f"/api/v1/sessions/{self.id}")


class Sessions(_Base):
    def create(
        self,
        *,
        profile: str = "shell",
        env: Optional[dict] = None,
        timeout_secs: Optional[int] = None,
        alias: Optional[str] = None,
        metadata: Optional[dict] = None,
        from_snapshot: Optional[str] = None,
        volumes: Optional[list[dict]] = None,
    ) -> Session:
        # cr-083 P1a: cr-085 create 高参(timeout_secs/alias/metadata)。注意:server 无 cwd。
        body: dict = {"profile_name": profile}
        if env:
            body["env"] = dict(env)
        if timeout_secs is not None:
            body["timeout_secs"] = timeout_secs
        if alias is not None:
            body["alias"] = alias
        if metadata:
            body["metadata"] = dict(metadata)
        if from_snapshot:
            body["from_snapshot"] = from_snapshot
        if volumes:
            body["volumes"] = volumes
        r = self._c._post("/api/v1/sessions", json=body)
        return Session(self._c, r["session_id"])

    def list(self) -> list[SessionInfo]:
        r = self._c._get("/api/v1/sessions")
        return [SessionInfo.from_json(s) for s in r.get("sessions", [])]

    def get(self, session_id: str) -> Session:
        return Session(self._c, session_id)

    def destroy(self, session_id: str) -> dict:
        return self._c._delete(f"/api/v1/sessions/{session_id}")


class Volumes(_Base):
    def create(self, name: str) -> dict:
        return self._c._post("/api/v1/volumes", json={"name": name})

    def list(self) -> list[str]:
        return self._c._get("/api/v1/volumes").get("volumes", [])

    def delete(self, name: str) -> dict:
        return self._c._delete(f"/api/v1/volumes/{name}")


class Client:
    """Client for a lv-sandbox server."""

    def __init__(
        self,
        base_url: str = "http://127.0.0.1:8080",
        *,
        api_key: Optional[str] = None,
        timeout: float = 300.0,
        transport: Any = None,
    ):
        headers = {"accept": "application/json"}
        if api_key:
            headers["Authorization"] = f"Bearer {api_key}"
        self._timeout = timeout
        # cr-083 P1a: transport= 注入缝(MockTransport 单测);None 走默认 HTTP transport。
        kwargs: dict = {
            "base_url": base_url.rstrip("/"),
            "headers": headers,
            "timeout": timeout,
        }
        if transport is not None:
            kwargs["transport"] = transport
        self._http = httpx.Client(**kwargs)
        self.jobs = Jobs(self)
        self.sessions = Sessions(self)
        self.volumes = Volumes(self)

    # ----- low-level helpers -----
    def _get(self, path: str, **kw) -> dict:
        resp = self._http.get(path, **kw)
        _raise_for_status(resp)
        return resp.json()

    def _get_raw(self, path: str, **kw) -> bytes:
        resp = self._http.get(path, **kw)
        _raise_for_status(resp)
        return resp.content

    def _post(self, path: str, **kw) -> dict:
        resp = self._http.post(path, **kw)
        _raise_for_status(resp)
        try:
            return resp.json()
        except Exception:
            return {}

    def _put(self, path: str, **kw) -> dict:
        resp = self._http.put(path, **kw)
        _raise_for_status(resp)
        try:
            return resp.json()
        except Exception:
            return {}

    def _delete(self, path: str, **kw) -> dict:
        resp = self._http.delete(path, **kw)
        _raise_for_status(resp)
        try:
            return resp.json()
        except Exception:
            return {}

    def _patch(self, path: str, **kw) -> dict:
        # cr-083 P1b: PATCH helper(set_timeout/set_metadata/set_alias)。
        resp = self._http.patch(path, **kw)
        _raise_for_status(resp)
        try:
            return resp.json()
        except Exception:
            return {}

    # ----- worker introspection -----
    def status(self) -> dict:
        return self._get("/api/v1/status")

    def profiles(self) -> list[str]:
        return self._get("/api/v1/profiles").get("profiles", [])

    # ----- cr-035: code-interpreter / agent-framework helpers -----
    def run_python(self, code, session=None, *, timeout="60s", profile="python"):
        """Write *code* → exec Python → return ``(result, workspace_files)``."""
        from .tools import run_python as _rp
        return _rp(self, code, session, timeout=timeout, profile=profile)

    def openai_tool_schema(self):
        """JSON schema for OpenAI function-calling tool definition."""
        from .tools import openai_tool_schema
        return openai_tool_schema()

    def langchain_tool(self, profile="python"):
        """LangChain BaseTool wrapping run_python (requires langchain-core)."""
        from .tools import langchain_tool
        return langchain_tool(self, profile)

    def close(self) -> None:
        self._http.close()

    def __enter__(self) -> "Client":
        return self

    def __exit__(self, *args) -> None:
        self.close()


# ---------------------------------------------------------------------------
# cr-083 P2: Async client subset(sessions/exec/files/snapshot;no jobs/volumes)
# ---------------------------------------------------------------------------


class _AsyncBase:
    def __init__(self, client: "AsyncClient"):
        self._c = client


class AsyncSessionFiles(_AsyncBase):
    def __init__(self, client: "AsyncClient", sid: str):
        super().__init__(client)
        self._sid = sid

    async def get(self, path: str) -> bytes:
        resp = await self._c._http.get(f"/api/v1/sessions/{self._sid}/files/{path}")
        _raise_for_status(resp)
        return resp.content

    async def put(self, path: str, data: bytes) -> dict:
        return await self._c._put(
            f"/api/v1/sessions/{self._sid}/files/{path}", content=data
        )

    async def list(self, path: str = "") -> list:
        r = await self._c._get(
            f"/api/v1/sessions/{self._sid}/files",
            params={"path": path} if path else {},
        )
        return [FileEntry.from_json(e) for e in r.get("entries", [])]

    async def delete(self, path: str) -> dict:
        return await self._c._delete(f"/api/v1/sessions/{self._sid}/files/{path}")

    async def make_dir(self, path: str) -> dict:
        return await self._c._post(
            f"/api/v1/sessions/{self._sid}/mkdir", json={"path": path}
        )

    async def exists(self, path: str) -> bool:
        resp = await self._c._http.head(f"/api/v1/sessions/{self._sid}/files/{path}")
        if resp.status_code == 404:
            return False
        _raise_for_status(resp)
        return resp.status_code < 400

    async def find(self, pattern: str, *, path: str = "", limit: Optional[int] = None):
        body: dict = {"pattern": pattern}
        if path:
            body["path"] = path
        if limit is not None:
            body["limit"] = limit
        r = await self._c._post(f"/api/v1/sessions/{self._sid}/files/find", json=body)
        return [FoundFile.from_json(f) for f in r.get("files", [])]

    async def search(self, pattern: str, *, path: str = ""):
        body = {"pattern": pattern}
        if path:
            body["path"] = path
        r = await self._c._post(
            f"/api/v1/sessions/{self._sid}/files/search", json=body
        )
        return [SearchHit.from_json(h) for h in r.get("results", [])]


class AsyncSession:
    def __init__(self, client: "AsyncClient", session_id: str):
        self._c = client
        self.id = session_id
        self.files = AsyncSessionFiles(client, session_id)

    async def info(self) -> SessionInfo:
        return SessionInfo.from_json(await self._c._get(f"/api/v1/sessions/{self.id}"))

    async def exec(
        self,
        argv,
        *,
        timeout: Optional[str] = None,
        env: Optional[dict] = None,
        stdin: Optional[str] = None,
        cwd: Optional[str] = None,
    ) -> JobResult:
        body: dict = {"argv": list(argv)}
        if timeout is not None:
            body["timeout"] = timeout
        if env:
            body["custom_env"] = dict(env)
        if stdin is not None:
            body["stdin"] = stdin
        if cwd is not None:
            body["cwd"] = cwd
        return JobResult.from_json(
            await self._c._post(f"/api/v1/sessions/{self.id}/exec", json=body)
        )

    async def exec_stream(
        self,
        argv,
        *,
        timeout: Optional[str] = None,
        env: Optional[dict] = None,
        stdin: Optional[str] = None,
        cwd: Optional[str] = None,
    ):
        """cr-083 P2: stream a session exec over SSE (yields StreamEvent)."""
        body: dict = {"argv": list(argv)}
        if timeout is not None:
            body["timeout"] = timeout
        if env:
            body["custom_env"] = dict(env)
        if stdin is not None:
            body["stdin"] = stdin
        if cwd is not None:
            body["cwd"] = cwd
        async with self._c._http.stream(
            "POST",
            f"/api/v1/sessions/{self.id}/exec?stream=true",
            json=body,
            timeout=self._c._timeout,
        ) as resp:
            _raise_for_status(resp)
            async for ev in iter_sse_async(resp):
                yield ev

    async def set_timeout(self, timeout_secs: int) -> dict:
        return await self._c._patch(
            f"/api/v1/sessions/{self.id}", json={"timeout_secs": timeout_secs}
        )

    async def set_metadata(self, metadata: dict) -> dict:
        return await self._c._patch(
            f"/api/v1/sessions/{self.id}", json={"metadata": dict(metadata)}
        )

    async def set_alias(self, alias: str) -> dict:
        return await self._c._patch(f"/api/v1/sessions/{self.id}", json={"alias": alias})

    async def watch(self, path: str = "", *, timeout_secs: int = 60):
        """cr-083 P2: watch via SSE. Async iterator yielding {event, paths}."""
        url = f"/api/v1/sessions/{self.id}/files/watch"
        params: dict = {"timeout_secs": timeout_secs}
        if path:
            params["path"] = path
        async with self._c._http.stream(
            "GET", url, params=params, timeout=self._c._timeout
        ) as resp:
            _raise_for_status(resp)
            async for ev in iter_sse_async(resp):
                paths = ev.data.get("paths") if isinstance(ev.data, dict) else []
                yield {"event": ev.type, "paths": paths or []}

    async def snapshot(self) -> str:
        return (await self._c._post(f"/api/v1/sessions/{self.id}/snapshot"))[
            "snapshot_id"
        ]

    async def destroy(self) -> dict:
        return await self._c._delete(f"/api/v1/sessions/{self.id}")


class AsyncSessions(_AsyncBase):
    async def create(
        self,
        *,
        profile: str = "shell",
        env: Optional[dict] = None,
        timeout_secs: Optional[int] = None,
        alias: Optional[str] = None,
        metadata: Optional[dict] = None,
        from_snapshot: Optional[str] = None,
        volumes: Optional[list[dict]] = None,
    ) -> AsyncSession:
        body: dict = {"profile_name": profile}
        if env:
            body["env"] = dict(env)
        if timeout_secs is not None:
            body["timeout_secs"] = timeout_secs
        if alias is not None:
            body["alias"] = alias
        if metadata:
            body["metadata"] = dict(metadata)
        if from_snapshot:
            body["from_snapshot"] = from_snapshot
        if volumes:
            body["volumes"] = volumes
        r = await self._c._post("/api/v1/sessions", json=body)
        return AsyncSession(self._c, r["session_id"])

    async def list(self) -> list:
        r = await self._c._get("/api/v1/sessions")
        return [SessionInfo.from_json(s) for s in r.get("sessions", [])]

    async def get(self, session_id: str) -> AsyncSession:
        return AsyncSession(self._c, session_id)

    async def destroy(self, session_id: str) -> dict:
        return await self._c._delete(f"/api/v1/sessions/{session_id}")


class AsyncClient:
    """Async client for a lv-sandbox server (cr-083 P2;subset)。

    镜像 ``Client`` 的 sessions/exec/files/snapshot 子集;不含 jobs/volumes。
    供 ``lvsandbox_e2b.AsyncSandbox`` 使用。
    """

    def __init__(
        self,
        base_url: str = "http://127.0.0.1:8080",
        *,
        api_key: Optional[str] = None,
        timeout: float = 300.0,
        transport: Any = None,
    ):
        headers = {"accept": "application/json"}
        if api_key:
            headers["Authorization"] = f"Bearer {api_key}"
        self._timeout = timeout
        kwargs: dict = {
            "base_url": base_url.rstrip("/"),
            "headers": headers,
            "timeout": timeout,
        }
        if transport is not None:
            kwargs["transport"] = transport
        self._http = httpx.AsyncClient(**kwargs)
        self.sessions = AsyncSessions(self)

    async def _get(self, path: str, **kw) -> dict:
        resp = await self._http.get(path, **kw)
        _raise_for_status(resp)
        return resp.json()

    async def _post(self, path: str, **kw) -> dict:
        resp = await self._http.post(path, **kw)
        _raise_for_status(resp)
        try:
            return resp.json()
        except Exception:
            return {}

    async def _put(self, path: str, **kw) -> dict:
        resp = await self._http.put(path, **kw)
        _raise_for_status(resp)
        try:
            return resp.json()
        except Exception:
            return {}

    async def _delete(self, path: str, **kw) -> dict:
        resp = await self._http.delete(path, **kw)
        _raise_for_status(resp)
        try:
            return resp.json()
        except Exception:
            return {}

    async def _patch(self, path: str, **kw) -> dict:
        resp = await self._http.patch(path, **kw)
        _raise_for_status(resp)
        try:
            return resp.json()
        except Exception:
            return {}

    async def aclose(self) -> None:
        await self._http.aclose()

    async def __aenter__(self) -> "AsyncClient":
        return self

    async def __aexit__(self, *args) -> None:
        await self.aclose()
