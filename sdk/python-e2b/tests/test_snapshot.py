"""cr-083 S5: Snapshot(SnapshotInfo 元数据部分支持——server 仅返裸 id)。"""
import json

import httpx

from lvsandbox_e2b import Sandbox


def _sb(handler):
    return Sandbox.connect(
        "s1", base_url="http://test", _transport=httpx.MockTransport(handler)
    )


def test_create_returns_snapshot_id():
    seen = {}

    def handler(req: httpx.Request):
        seen.update(method=req.method, path=req.url.path)
        return httpx.Response(201, json={"snapshot_id": "snap-1"})

    sb = _sb(handler)
    assert sb.snapshot.create() == "snap-1"
    assert seen == {"method": "POST", "path": "/api/v1/sessions/s1/snapshot"}


def test_list_maps_bare_ids_to_snapshot_info():
    def handler(req: httpx.Request):
        return httpx.Response(200, json={"snapshots": ["snap-1", "snap-2"]})

    sb = _sb(handler)
    res = sb.snapshot.list()
    assert [s.snapshot_id for s in res] == ["snap-1", "snap-2"]


def test_delete_removes_snapshot():
    seen = {}

    def handler(req: httpx.Request):
        seen.update(method=req.method, path=req.url.path)
        return httpx.Response(200, json={"ok": True, "snapshot_id": "snap-1"})

    sb = _sb(handler)
    sb.snapshot.delete("snap-1")
    assert seen == {"method": "DELETE", "path": "/api/v1/snapshots/snap-1"}
