"""Integration tests for the lv-sandbox Python SDK (need a running server)."""
import pytest

pytestmark = pytest.mark.integration


def test_status_and_profiles(client):
    assert client.profiles()  # built-ins present
    assert "shell" in client.profiles()


def test_job_run(client):
    r = client.jobs.run(["/bin/echo", "hello sdk"], profile="shell", timeout="5s")
    assert r.status == "Completed"
    assert r.exit_code == 0
    assert "hello sdk" in r.stdout


def test_job_stream(client):
    types = [ev.type for ev in client.jobs.stream(["/bin/echo", "x"], profile="shell")]
    assert "started" in types
    assert "stdout" in types
    assert "result" in types


def test_session_exec_and_files(client):
    s = client.sessions.create(profile="shell")
    try:
        # upload/download roundtrip
        s.files.put("in.txt", b"sdk-data")
        assert s.files.get("in.txt") == b"sdk-data"
        # cross-exec persistence via builtin-only echo (no fork — nproc-safe in
        # dev env where per-uid process limits bite fork-heavy commands)
        s.exec(["/bin/sh", "-c", "echo first > shared.txt"])
        s.exec(["/bin/sh", "-c", "echo second >> shared.txt"])
        assert s.files.get("shared.txt") == b"first\nsecond\n"
        # list + delete
        names = [e.name for e in s.files.list()]
        assert "in.txt" in names and "shared.txt" in names
        s.files.delete("in.txt")
        assert "in.txt" not in [e.name for e in s.files.list()]
    finally:
        s.destroy()


def test_snapshot_fork(client):
    s = client.sessions.create(profile="shell")
    try:
        s.files.put("f.txt", b"snap-me")
        snap = s.snapshot()
        s2 = client.sessions.create(profile="shell", from_snapshot=snap)
        try:
            assert s2.files.get("f.txt") == b"snap-me"
        finally:
            s2.destroy()
    finally:
        s.destroy()


def test_volume_persists_across_sessions(client):
    client.volumes.create("sdkvol")
    try:
        a = client.sessions.create(
            profile="shell", volumes=[{"name": "sdkvol", "mount": "volumes/d"}]
        )
        a.exec(["/bin/sh", "-c", "echo vol > volumes/d/x"])
        a.destroy()
        b = client.sessions.create(
            profile="shell", volumes=[{"name": "sdkvol", "mount": "volumes/d"}]
        )
        try:
            assert b.files.get("volumes/d/x") == b"vol\n"
        finally:
            b.destroy()
    finally:
        client.volumes.delete("sdkvol")


# ----- cr-035: agent-framework integrations -----

def test_openai_tool_schema():
    """Unit test — no server needed."""
    from lvsandbox import openai_tool_schema

    schema = openai_tool_schema()
    assert schema["type"] == "function"
    assert schema["function"]["name"] == "run_python"
    assert "code" in schema["function"]["parameters"]["properties"]


def test_run_python(client):
    """Integration — needs server + python3 in the image/machine."""
    r, files = client.run_python("print('from-python-sdk')")
    assert r.status == "Completed"
    assert "from-python-sdk" in r.stdout
