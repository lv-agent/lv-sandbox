"""Data models returned by the SDK."""
from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Optional


@dataclass
class JobResult:
    """Result of a one-shot job or a session exec."""

    job_id: str
    status: str
    exit_code: Optional[int] = None
    signal: Optional[int] = None
    stdout: str = ""
    stderr: str = ""
    duration_ms: int = 0
    timed_out: bool = False

    @classmethod
    def from_json(cls, d: dict) -> "JobResult":
        return cls(
            job_id=d.get("job_id", ""),
            status=d.get("status", ""),
            exit_code=d.get("exit_code"),
            signal=d.get("signal"),
            stdout=d.get("stdout") or "",
            stderr=d.get("stderr") or "",
            duration_ms=d.get("duration_ms") or 0,
            timed_out=bool(d.get("timed_out", False)),
        )


# Session exec returns the same shape as a job result.
ExecResult = JobResult


@dataclass
class StreamEvent:
    """One SSE event from a streaming exec.

    `type` is "started" / "stdout" / "result". For "stdout" events, `.stdout`
    holds the chunk text; for "result", `.result` parses the final JobResult.
    """

    type: str
    data: Any
    job_id: Optional[str] = None
    stdout: Optional[str] = None

    @property
    def result(self) -> Optional[JobResult]:
        if self.type == "result" and isinstance(self.data, dict):
            return JobResult.from_json(self.data)
        return None


@dataclass
class SessionInfo:
    session_id: str
    profile: str = ""
    created_at_secs: int = 0
    last_activity_secs: int = 0
    execs: int = 0

    @classmethod
    def from_json(cls, d: dict) -> "SessionInfo":
        return cls(
            session_id=d.get("session_id", ""),
            profile=d.get("profile", ""),
            created_at_secs=d.get("created_at_secs") or 0,
            last_activity_secs=d.get("last_activity_secs") or 0,
            execs=d.get("execs") or 0,
        )


@dataclass
class FileEntry:
    name: str
    size: int
    is_dir: bool

    @classmethod
    def from_json(cls, d: dict) -> "FileEntry":
        return cls(
            name=d.get("name", ""),
            size=d.get("size") or 0,
            is_dir=bool(d.get("is_dir", False)),
        )
