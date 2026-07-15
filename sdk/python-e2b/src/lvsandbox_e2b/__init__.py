"""lvsandbox-e2b: E2B API-compatible shim over lv-sandbox (cr-083).

Surface aligned with the E2B Python SDK (class/method/param/exception names),
backed by the lv-sandbox HTTP API via the `lvsandbox` SDK. API-surface
compatible, not wire-compatible.

    from lvsandbox_e2b import Sandbox, AsyncSandbox
"""
from .exceptions import (
    AuthenticationException,
    CommandException,
    CommandExitException,
    E2BError,
    FileException,
    FileNotFoundException,
    PermissionDeniedError,
    PTYException,
    RateLimitException,
    SandboxException,
    SandboxNotRunningError,
    TemplateException,
    TimeoutException,
)
from .models import FileInfo, Process, SandboxInfo, SnapshotInfo
from .sandbox import AsyncSandbox, Sandbox

__all__ = [
    # core
    "Sandbox",
    "AsyncSandbox",
    # models
    "SandboxInfo",
    "FileInfo",
    "Process",
    "SnapshotInfo",
    # exceptions
    "E2BError",
    "SandboxException",
    "TimeoutException",
    "SandboxNotRunningError",
    "FileException",
    "FileNotFoundException",
    "PermissionDeniedError",
    "CommandException",
    "CommandExitException",
    "TemplateException",
    "PTYException",
    "AuthenticationException",
    "RateLimitException",
]

__version__ = "0.1.0"
