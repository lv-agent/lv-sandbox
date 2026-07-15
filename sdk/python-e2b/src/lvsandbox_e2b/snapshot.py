"""E2B-shaped Snapshot (cr-083 §6.2).

SnapshotInfo 元数据部分支持:lv-sandbox server 的 snapshot.list 仅返回裸 id
数组(cr-085 未做 SnapshotInfo),故除 snapshot_id 外字段不可用(design §9)。
"""
from __future__ import annotations

from ._handle import translating
from .models import SnapshotInfo


class Snapshot:
    def __init__(self, client, sandbox_id: str):
        self._c = client
        self._sid = sandbox_id

    def create(self) -> str:
        with translating("session"):
            return self._c.sessions.get(self._sid).snapshot()

    def list(self) -> list:
        # server: GET /api/v1/snapshots -> {"snapshots": ["id", ...]}(裸 id)
        with translating("session"):
            r = self._c._get("/api/v1/snapshots")
        return [SnapshotInfo(snapshot_id=i) for i in r.get("snapshots", [])]

    def delete(self, snapshot_id: str) -> None:
        with translating("session"):
            self._c._delete(f"/api/v1/snapshots/{snapshot_id}")
