"""Layer 0 (cr-083 P1a/P1b/P1c/P2): cr-085 SDK surface —— mock 单测。

用 httpx.MockTransport 注入 Client(transport=),无需 server。
"""
import asyncio
import json

import httpx

from lvsandbox import Client


def _client(handler):
    return Client("http://test", transport=httpx.MockTransport(handler))


# ----- P1a: create/exec 新字段 -----


def test_create_sends_timeout_secs_alias_metadata():
    captured = {}

    def handler(req: httpx.Request):
        captured["body"] = json.loads(req.content)
        return httpx.Response(201, json={"session_id": "s1"})

    c = _client(handler)
    s = c.sessions.create(
        profile="python", timeout_secs=120, alias="a1", metadata={"k": "v"}
    )
    assert s.id == "s1"
    body = captured["body"]
    assert body["profile_name"] == "python"
    assert body["timeout_secs"] == 120
    assert body["alias"] == "a1"
    assert body["metadata"] == {"k": "v"}
    assert "cwd" not in body  # create 无 cwd


def test_exec_sends_cwd_and_custom_env():
    captured = {}

    def handler(req: httpx.Request):
        captured["body"] = json.loads(req.content)
        return httpx.Response(
            200,
            json={
                "job_id": "session:s1",
                "status": "Completed",
                "exit_code": 0,
                "stdout": "hi",
                "stderr": "",
                "duration_ms": 5,
                "timed_out": False,
            },
        )

    c = _client(handler)
    s = c.sessions.get("s1")
    r = s.exec(["/bin/sh", "-c", "echo hi"], cwd="sub", env={"E": "1"})
    body = captured["body"]
    assert body["cwd"] == "sub"
    assert body["custom_env"] == {"E": "1"}
    assert r.exit_code == 0
    assert r.stdout == "hi"


# ----- P1b: PATCH 生命周期 + watch SSE -----


def test_set_timeout_metadata_alias_patch():
    seen = []

    def handler(req: httpx.Request):
        seen.append((req.method, req.url.path, json.loads(req.content)))
        return httpx.Response(200, json={"ok": True})

    c = _client(handler)
    s = c.sessions.get("s1")
    s.set_timeout(300)
    s.set_metadata({"x": "1"})
    s.set_alias("nm")
    assert seen[0] == ("PATCH", "/api/v1/sessions/s1", {"timeout_secs": 300})
    assert seen[1] == ("PATCH", "/api/v1/sessions/s1", {"metadata": {"x": "1"}})
    assert seen[2] == ("PATCH", "/api/v1/sessions/s1", {"alias": "nm"})


def test_watch_yields_created_modified_removed():
    body = (
        b'event: created\ndata: {"paths":["a.txt"]}\n\n'
        b'event: modified\ndata: {"paths":["a.txt"]}\n\n'
        b'event: removed\ndata: {"paths":["b.txt"]}\n\n'
    )

    def handler(req: httpx.Request):
        return httpx.Response(
            200, headers={"content-type": "text/event-stream"}, content=body
        )

    c = _client(handler)
    s = c.sessions.get("s1")
    events = list(s.watch(timeout_secs=5))
    assert events == [
        {"event": "created", "paths": ["a.txt"]},
        {"event": "modified", "paths": ["a.txt"]},
        {"event": "removed", "paths": ["b.txt"]},
    ]


# ----- P1c: mkdir/exists/find/search + 富模型 -----


def test_make_dir_posts_path():
    seen = {}

    def handler(req: httpx.Request):
        seen.update(method=req.method, path=req.url.path, body=json.loads(req.content))
        return httpx.Response(200, json={"ok": True})

    c = _client(handler)
    s = c.sessions.get("s1")
    assert s.files.make_dir("a/b") == {"ok": True}
    assert seen["method"] == "POST"
    assert seen["path"] == "/api/v1/sessions/s1/mkdir"
    assert seen["body"] == {"path": "a/b"}


def test_exists_200_true_404_false():
    def handler(req: httpx.Request):
        return httpx.Response(200 if req.url.path.endswith("/yes") else 404)

    c = _client(handler)
    s = c.sessions.get("s1")
    assert s.files.exists("yes") is True
    assert s.files.exists("no") is False


def test_find_parses_files_with_path_and_entry():
    payload = {
        "files": [
            {
                "path": "sub/a.py",
                "entry": {"name": "a.py", "size": 10, "is_dir": False, "mode": 33188},
            }
        ],
        "truncated": False,
    }

    def handler(req: httpx.Request):
        return httpx.Response(200, json=payload)

    c = _client(handler)
    s = c.sessions.get("s1")
    found = s.files.find("**/*.py")
    assert found[0].path == "sub/a.py"
    assert found[0].entry.name == "a.py"
    assert found[0].entry.mode == 0o100644


def test_search_parses_results_with_matches():
    payload = {
        "results": [{"path": "code.py", "matches": [{"line": 2, "text": "TODO: fix"}]}],
        "truncated": True,
    }

    def handler(req: httpx.Request):
        return httpx.Response(200, json=payload)

    c = _client(handler)
    s = c.sessions.get("s1")
    res = s.files.search("TODO")
    assert res[0].path == "code.py"
    assert res[0].matches[0].line == 2
    assert res[0].matches[0].text == "TODO: fix"


# ----- P2: AsyncClient 子集(asyncio.run 包装,免 pytest-asyncio 依赖) -----


def _async_client(handler):
    from lvsandbox import AsyncClient

    return AsyncClient("http://test", transport=httpx.MockTransport(handler))


def test_async_create_exec_read():
    async def run():
        def handler(req: httpx.Request):
            p = req.url.path
            if p == "/api/v1/sessions":
                return httpx.Response(201, json={"session_id": "s1"})
            if p.endswith("/exec"):
                return httpx.Response(
                    200,
                    json={
                        "job_id": "session:s1",
                        "status": "Completed",
                        "exit_code": 0,
                        "stdout": "out",
                        "stderr": "",
                        "duration_ms": 1,
                        "timed_out": False,
                    },
                )
            if p.endswith("/files/f.txt"):
                return httpx.Response(200, content=b"hello")
            return httpx.Response(404, json={"error": "no"})

        c = _async_client(handler)
        s = await c.sessions.create(profile="python", timeout_secs=60)
        assert s.id == "s1"
        r = await s.exec(["echo hi"], cwd=".")
        assert r.exit_code == 0 and r.stdout == "out"
        data = await s.files.get("f.txt")
        assert data == b"hello"
        await c.aclose()

    asyncio.run(run())


def test_async_exec_stream_sse():
    async def run():
        body = (
            b'event: started\ndata: {"job_id":"x"}\n\n'
            b'event: stdout\ndata: {"data":"hi"}\n\n'
            b'event: result\ndata: {"job_id":"x","status":"Completed","exit_code":0,'
            b'"stdout":[],"stderr":[],"duration":{"secs":0,"nanos":1},"timed_out":false}\n\n'
        )

        def handler(req: httpx.Request):
            return httpx.Response(
                200, headers={"content-type": "text/event-stream"}, content=body
            )

        c = _async_client(handler)
        s = await c.sessions.get("s1")
        types = []
        async for ev in s.exec_stream(["echo hi"]):
            types.append(ev.type)
        assert types == ["started", "stdout", "result"]
        await c.aclose()

    asyncio.run(run())


def test_async_find_and_exists():
    async def run():
        def handler(req: httpx.Request):
            if req.method == "POST":
                return httpx.Response(
                    200,
                    json={
                        "files": [
                            {
                                "path": "a.py",
                                "entry": {"name": "a.py", "size": 1, "is_dir": False},
                            }
                        ],
                        "truncated": False,
                    },
                )
            return httpx.Response(200 if req.url.path.endswith("/yes") else 404)

        c = _async_client(handler)
        s = await c.sessions.get("s1")
        found = await s.files.find("*.py")
        assert found[0].path == "a.py"
        assert await s.files.exists("yes") is True
        assert await s.files.exists("no") is False
        await c.aclose()

    asyncio.run(run())


def test_async_watch_sse():
    async def run():
        body = (
            b'event: created\ndata: {"paths":["a.txt"]}\n\n'
            b'event: removed\ndata: {"paths":["b.txt"]}\n\n'
        )

        def handler(req: httpx.Request):
            return httpx.Response(
                200, headers={"content-type": "text/event-stream"}, content=body
            )

        c = _async_client(handler)
        s = await c.sessions.get("s1")
        out = []
        async for ev in s.watch(timeout_secs=5):
            out.append(ev)
        assert out == [
            {"event": "created", "paths": ["a.txt"]},
            {"event": "removed", "paths": ["b.txt"]},
        ]
        await c.aclose()

    asyncio.run(run())


def test_async_set_timeout_patch():
    async def run():
        seen = {}

        def handler(req: httpx.Request):
            seen.update(method=req.method, body=json.loads(req.content))
            return httpx.Response(200, json={"ok": True})

        c = _async_client(handler)
        s = await c.sessions.get("s1")
        await s.set_timeout(90)
        assert seen["method"] == "PATCH" and seen["body"] == {"timeout_secs": 90}
        await c.aclose()

    asyncio.run(run())
