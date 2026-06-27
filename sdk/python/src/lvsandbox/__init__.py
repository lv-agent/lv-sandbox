"""lv-sandbox Python SDK.

A thin client for the lv-sandbox HTTP API: one-shot jobs, persistent sessions
(with files, snapshots, volumes), streaming stdout, and worker introspection.

    from lvsandbox import Client

    lv = Client("http://127.0.0.1:8080")
    print(lv.jobs.run(["/bin/echo", "hi"], profile="shell").stdout)

    s = lv.sessions.create(profile="shell")
    s.files.put("run.sh", b"echo hello")
    print(s.exec(["/bin/sh", "run.sh"]).stdout)
"""

from .client import Client, Jobs, Session, Sessions, Volumes
from .errors import LvApiError, LvError
from .models import ExecResult, FileEntry, JobResult, SessionInfo, StreamEvent
from .tools import openai_tool_schema

__all__ = [
    "Client",
    "Jobs",
    "Session",
    "Sessions",
    "Volumes",
    "JobResult",
    "ExecResult",
    "StreamEvent",
    "SessionInfo",
    "FileEntry",
    "LvError",
    "LvApiError",
    "openai_tool_schema",
]
__version__ = "0.3.0"
