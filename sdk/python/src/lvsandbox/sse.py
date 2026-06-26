"""Minimal SSE (Server-Sent Events) parser for streaming exec responses.

Stays dependency-free (parses the ``text/event-stream`` body line by line)."""
from __future__ import annotations

import json
from typing import Any, Iterator

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
