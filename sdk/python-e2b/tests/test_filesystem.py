"""cr-083 S4: Filesystem 全套。"""
import json

import httpx

from lvsandbox_e2b import Sandbox


def _sb(handler):
    return Sandbox.connect(
        "s1", base_url="http://test", _transport=httpx.MockTransport(handler)
    )


def test_read_text_and_bytes():
    def handler(req: httpx.Request):
        if req.method == "GET" and req.url.path.endswith("/files/test.txt"):
            return httpx.Response(200, content=b"hello")
        return httpx.Response(404, json={"error": "no"})

    sb = _sb(handler)
    assert sb.filesystem.read("/test.txt") == "hello"
    assert sb.filesystem.read("/test.txt", format="bytes") == b"hello"


def test_write_puts_bytes_and_returns_fileinfo():
    seen = {}

    def handler(req: httpx.Request):
        if req.method == "PUT":
            seen["body"] = req.content
            return httpx.Response(200, json={"ok": True})
        return httpx.Response(404, json={"error": "no"})

    sb = _sb(handler)
    fi = sb.filesystem.write("/test.txt", b"hello")
    assert fi.name == "test.txt"
    assert seen["body"] == b"hello"


def test_list_maps_fileinfo_with_mode():
    payload = {
        "entries": [{"name": "a.txt", "size": 5, "is_dir": False, "mode": 33188}]
    }

    def handler(req: httpx.Request):
        return httpx.Response(200, json=payload)

    sb = _sb(handler)
    items = sb.filesystem.list("/")
    assert items[0].name == "a.txt"
    assert items[0].mode == 0o100644


def test_remove_deletes():
    seen = {}

    def handler(req: httpx.Request):
        seen.update(method=req.method, path=req.url.path)
        return httpx.Response(200, json={"ok": True})

    sb = _sb(handler)
    sb.filesystem.remove("/test.txt")
    assert seen == {"method": "DELETE", "path": "/api/v1/sessions/s1/files/test.txt"}


def test_make_dir_returns_dir_fileinfo():
    seen = {}

    def handler(req: httpx.Request):
        seen.update(method=req.method, body=json.loads(req.content))
        return httpx.Response(200, json={"ok": True})

    sb = _sb(handler)
    fi = sb.filesystem.make_dir("/sub")
    assert fi.is_dir is True
    assert fi.name == "sub"
    assert seen["method"] == "POST"


def test_exists_true_false():
    def handler(req: httpx.Request):
        return httpx.Response(200 if req.url.path.endswith("/yes") else 404)

    sb = _sb(handler)
    assert sb.filesystem.exists("/yes") is True
    assert sb.filesystem.exists("/no") is False


def test_find_uses_path_as_name():
    payload = {
        "files": [
            {"path": "sub/a.py", "entry": {"name": "a.py", "size": 1, "is_dir": False}}
        ],
        "truncated": False,
    }

    def handler(req: httpx.Request):
        return httpx.Response(200, json=payload)

    sb = _sb(handler)
    found = sb.filesystem.find("/", "**/*.py")
    assert found[0].name == "sub/a.py"  # workspace 相对全路径作 name


def test_search_passthrough():
    payload = {
        "results": [{"path": "code.py", "matches": [{"line": 2, "text": "TODO"}]}],
        "truncated": False,
    }

    def handler(req: httpx.Request):
        return httpx.Response(200, json=payload)

    sb = _sb(handler)
    res = sb.filesystem.search("/", "TODO")
    assert res[0].path == "code.py"
    assert res[0].matches[0].text == "TODO"


def test_watch_dir_invokes_on_event():
    body = b'event: created\ndata: {"paths":["a.txt"]}\n\n'

    def handler(req: httpx.Request):
        return httpx.Response(
            200, headers={"content-type": "text/event-stream"}, content=body
        )

    sb = _sb(handler)
    events = []
    sb.filesystem.watch_dir("/", lambda e: events.append(e), timeout=5)
    assert events == [{"event": "created", "paths": ["a.txt"]}]
