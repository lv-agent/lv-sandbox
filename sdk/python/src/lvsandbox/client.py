"""HTTP client for lv-sandbox.

Mirrors the v0.3 HTTP API: one-shot jobs, persistent sessions (with files,
snapshots, volumes), streaming stdout (SSE), and worker introspection.
"""
from __future__ import annotations

import time
from typing import Any, Iterator, Optional

import httpx

from .errors import LvApiError
from .models import FileEntry, JobResult, SessionInfo, StreamEvent
from .sse import iter_sse


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
    ) -> JobResult:
        """Run a command in this session's persistent workspace (synchronous)."""
        body: dict = {"argv": list(argv)}
        if timeout is not None:
            body["timeout"] = timeout
        if env:
            body["custom_env"] = dict(env)
        if stdin is not None:
            body["stdin"] = stdin
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
        from_snapshot: Optional[str] = None,
        volumes: Optional[list[dict]] = None,
    ) -> Session:
        body: dict = {"profile_name": profile}
        if env:
            body["env"] = dict(env)
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
    ):
        headers = {"accept": "application/json"}
        if api_key:
            headers["Authorization"] = f"Bearer {api_key}"
        self._timeout = timeout
        self._http = httpx.Client(
            base_url=base_url.rstrip("/"), headers=headers, timeout=timeout
        )
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

    # ----- worker introspection -----
    def status(self) -> dict:
        return self._get("/api/v1/status")

    def profiles(self) -> list[str]:
        return self._get("/api/v1/profiles").get("profiles", [])

    def close(self) -> None:
        self._http.close()

    def __enter__(self) -> "Client":
        return self

    def __exit__(self, *args) -> None:
        self.close()
