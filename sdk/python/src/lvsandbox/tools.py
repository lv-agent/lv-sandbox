"""Agent framework integrations — code-interpreter helpers + tool definitions.

Provides:
- `Client.run_python(code)` — write code → exec → return result + file listing.
- `Client.openai_tool_schema()` — JSON schema for OpenAI function calling.
- `Client.langchain_tool()` — a LangChain BaseTool (requires langchain installed).
"""
from __future__ import annotations

from typing import Any, Optional, Tuple

from .client import Client
from .models import FileEntry, JobResult


def run_python(
    self: Client,
    code: str,
    session=None,
    *,
    timeout: str = "60s",
    profile: str = "python",
) -> Tuple[JobResult, list[FileEntry]]:
    """Write *code* to `_run.py` in the session workspace, exec it, and return
    ``(result, workspace_files)``. Creates a session if none given.
    """
    s = session or self.sessions.create(profile=profile)
    s.files.put("_run.py", code.encode())
    result = s.exec(["/usr/bin/python3", "_run.py"], timeout=timeout)
    files = s.files.list()
    return result, files


def openai_tool_schema() -> dict[str, Any]:
    """JSON schema for OpenAI / function-calling tool definition."""
    return {
        "type": "function",
        "function": {
            "name": "run_python",
            "description": (
                "Execute Python code in a sandboxed lv-sandbox environment. "
                "Returns stdout, stderr, exit code, and a list of generated files."
            ),
            "parameters": {
                "type": "object",
                "properties": {
                    "code": {
                        "type": "string",
                        "description": "Python code to execute",
                    },
                    "timeout": {
                        "type": "string",
                        "description": "Execution timeout, e.g. '30s'. Default 60s.",
                    },
                },
                "required": ["code"],
            },
        },
    }


def langchain_tool(client: Client, profile: str = "python"):
    """Return a LangChain BaseTool that runs Python in lv-sandbox.

    Requires ``langchain`` installed (``pip install langchain``).
    """
    try:
        from langchain_core.tools import BaseTool  # type: ignore
        from pydantic import BaseModel, Field  # type: ignore
    except ImportError:
        raise ImportError(
            "langchain_core is required: pip install langchain-core"
        )

    class _Input(BaseModel):
        code: str = Field(description="Python code to execute")
        timeout: str = Field(default="60s", description="Execution timeout")

    class _LvSandboxTool(BaseTool):
        name: str = "lv_sandbox_run_python"
        description: str = (
            "Execute Python code in a sandboxed lv-sandbox environment. "
            "Returns stdout, exit code, and generated file listing."
        )
        args_schema: type = _Input

        def _run(self, code: str, timeout: str = "60s") -> str:
            r, files = run_python(client, code, timeout=timeout, profile=profile)
            names = ", ".join(f.path for f in files)
            return f"exit={r.exit_code} stdout={r.stdout!r} files=[{names}]"

        async def _arun(self, code: str, timeout: str = "60s") -> str:
            return self._run(code, timeout)

    return _LvSandboxTool()
