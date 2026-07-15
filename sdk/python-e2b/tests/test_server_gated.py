"""cr-083 S7: server-gated 项 —— keep_alive 可行(activity-touch),其余 defer。"""
import json

import httpx
import pytest

from lvsandbox_e2b import Sandbox


def _sb(handler):
    return Sandbox.connect(
        "s1", base_url="http://test", _transport=httpx.MockTransport(handler)
    )


def test_keep_alive_patches_to_reset_activity():
    seen = {}

    def handler(req: httpx.Request):
        seen.update(method=req.method, body=json.loads(req.content))
        return httpx.Response(200, json={"ok": True})

    sb = _sb(handler)
    sb.keep_alive(5)
    # lv-sandbox PATCH 重置 last_activity(实际 keep-alive 机制,cr-040 全局 TTL reaper)
    assert seen["method"] == "PATCH"


def test_send_stdin_not_implemented():
    sb = _sb(lambda r: httpx.Response(200, json={"ok": True}))
    with pytest.raises(NotImplementedError):
        sb.commands.send_stdin(1, b"x")


def test_commands_list_and_kill_not_implemented():
    sb = _sb(lambda r: httpx.Response(200, json={"ok": True}))
    with pytest.raises(NotImplementedError):
        sb.commands.list()
    with pytest.raises(NotImplementedError):
        sb.commands.kill(1)


def test_get_metrics_not_implemented():
    sb = _sb(lambda r: httpx.Response(200, json={"ok": True}))
    with pytest.raises(NotImplementedError):
        sb.get_metrics()
