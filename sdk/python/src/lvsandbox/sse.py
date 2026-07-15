"""Minimal SSE (Server-Sent Events) parser for streaming exec responses.

Stays dependency-free (parses the ``text/event-stream`` body line by line)."""
from __future__ import annotations

import json
from typing import Any, AsyncIterator, Iterator

from .models import StreamEvent


def iter_sse(response) -> Iterator[StreamEvent]:
    """Yield :class:`StreamEvent` from an httpx streaming response."""
    event: str | None = None
    data_lines: list[str] = []
    for line in response.iter_lines():
        if line == "":
            # Blank line dispatches a message (if any data accumulated).
            if data_lines:
                raw = "\n".join(data_lines)
                try:
                    payload: Any = json.loads(raw)
                except json.JSONDecodeError:
                    payload = raw
                job_id = payload.get("job_id") if isinstance(payload, dict) else None
                stdout = (
                    payload.get("data")
                    if (event == "stdout" and isinstance(payload, dict))
                    else None
                )
                yield StreamEvent(
                    type=event or "message", data=payload, job_id=job_id, stdout=stdout
                )
            event = None
            data_lines = []
        elif line.startswith("event:"):
            event = line[len("event:"):].strip()
        elif line.startswith("data:"):
            data_lines.append(line[len("data:"):].lstrip())


async def iter_sse_async(response) -> AsyncIterator[StreamEvent]:
    """Async counterpart of :func:`iter_sse` for ``httpx`` async streaming.

    状态机与同步版一致(event/data 行累积,空行派发),仅迭代方式换成 ``aiter_lines``。
    cr-083 P2:供 AsyncSession.exec_stream / watch 使用。
    """
    event: str | None = None
    data_lines: list[str] = []
    async for line in response.aiter_lines():
        if line == "":
            if data_lines:
                raw = "\n".join(data_lines)
                try:
                    payload: Any = json.loads(raw)
                except json.JSONDecodeError:
                    payload = raw
                job_id = payload.get("job_id") if isinstance(payload, dict) else None
                stdout = (
                    payload.get("data")
                    if (event == "stdout" and isinstance(payload, dict))
                    else None
                )
                yield StreamEvent(
                    type=event or "message", data=payload, job_id=job_id, stdout=stdout
                )
            event = None
            data_lines = []
        elif line.startswith("event:"):
            event = line[len("event:"):].strip()
        elif line.startswith("data:"):
            data_lines.append(line[len("data:"):].lstrip())
