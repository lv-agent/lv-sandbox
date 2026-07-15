"""Data models returned by the SDK."""
from __future__ import annotations

from dataclasses import dataclass, field
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
    """Session info. Rich fields aligned with E2B SandboxInfo (cr-085 M2).

    New fields default so older servers (without them) still deserialize.
    """

    session_id: str
    profile: str = ""
    created_at_secs: int = 0
    last_activity_secs: int = 0
    execs: int = 0
    template_id: Optional[str] = None
    state: str = ""
    started_at: Optional[int] = None  # absolute unix seconds
    cpu_count: int = 0
    memory_size: int = 0  # bytes
    alias: Optional[str] = None
    timeout_secs: Optional[int] = None
    metadata: dict = field(default_factory=dict)

    @classmethod
    def from_json(cls, d: dict) -> "SessionInfo":
        return cls(
            session_id=d.get("session_id", ""),
            profile=d.get("profile", ""),
            created_at_secs=d.get("created_at_secs") or 0,
            last_activity_secs=d.get("last_activity_secs") or 0,
            execs=d.get("execs") or 0,
            template_id=d.get("template_id"),
            state=d.get("state", ""),
            started_at=d.get("started_at"),
            cpu_count=d.get("cpu_count") or 0,
            memory_size=d.get("memory_size") or 0,
            alias=d.get("alias"),
            timeout_secs=d.get("timeout_secs"),
            metadata=d.get("metadata") or {},
        )


@dataclass
class FileEntry:
    """One entry from `files.list()`.

    Rich metadata aligned with E2B FileInfo (cr-085 M1): mode / owner / group /
    timestamps / symlink target. New fields default so older servers (without
    them) still deserialize cleanly.
    """

    name: str
    size: int
    is_dir: bool
    is_symlink: bool = False
    mode: int = 0  # POSIX st_mode (includes file-type bits)
    owner: str = ""
    group: str = ""
    modified_at: Optional[int] = None  # unix seconds
    created_at: Optional[int] = None  # unix seconds (ctime; no birth time on Linux)
    symlink_target: Optional[str] = None

    @classmethod
    def from_json(cls, d: dict) -> "FileEntry":
        return cls(
            name=d.get("name", ""),
            size=d.get("size") or 0,
            is_dir=bool(d.get("is_dir", False)),
            is_symlink=bool(d.get("is_symlink", False)),
            mode=d.get("mode") or 0,
            owner=d.get("owner", ""),
            group=d.get("group", ""),
            modified_at=d.get("modified_at"),
            created_at=d.get("created_at"),
            symlink_target=d.get("symlink_target"),
        )
