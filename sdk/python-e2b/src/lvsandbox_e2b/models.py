"""E2B-shaped models (thin wrappers over lvsandbox models)."""
from __future__ import annotations

from dataclasses import dataclass, field
from typing import Optional


@dataclass
class SandboxInfo:
    """E2B SandboxInfo, mapped from lvsandbox SessionInfo."""

    sandbox_id: str
    state: str = ""
    template_id: Optional[str] = None
    alias: Optional[str] = None
    started_at: Optional[int] = None  # absolute unix seconds
    cpu_count: int = 0
    memory_size: int = 0
    metadata: dict = field(default_factory=dict)

    @classmethod
    def from_session_info(cls, d) -> "SandboxInfo":
        # d: lvsandbox.SessionInfo(created_at_secs/last_activity_secs 是 age,弃用;
        # started_at 是绝对 unix 秒——见 design §2.3)
        return cls(
            sandbox_id=d.session_id,
            state=d.state,
            template_id=d.template_id,
            alias=d.alias,
            started_at=d.started_at,
            cpu_count=d.cpu_count,
            memory_size=d.memory_size,
            metadata=getattr(d, "metadata", {}) or {},
        )


@dataclass
class FileInfo:
    """E2B FileInfo, mapped from lvsandbox FileEntry."""

    name: str
    is_dir: bool = False
    size: int = 0
    mode: int = 0
    owner: str = ""
    group: str = ""
    modified_at: Optional[int] = None
    created_at: Optional[int] = None
    symlink_target: Optional[str] = None

    @classmethod
    def from_file_entry(cls, e) -> "FileInfo":
        return cls(
            name=e.name,
            is_dir=e.is_dir,
            size=e.size,
            mode=e.mode,
            owner=e.owner,
            group=e.group,
            modified_at=e.modified_at,
            created_at=e.created_at,
            symlink_target=e.symlink_target,
        )


@dataclass
class Process:
    """Result of commands.run."""

    exit_code: Optional[int] = None
    stdout: bytes = b""
    stderr: bytes = b""
    error: Optional[str] = None


@dataclass
class SnapshotInfo:
    """E2B SnapshotInfo.

    Note: lv-sandbox server returns bare snapshot ids (no metadata); fields
    beyond snapshot_id are unavailable (partial support — design §9).
    """

    snapshot_id: str
