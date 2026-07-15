"""Integration tests against a live lv-sandbox server (cr-083 §8 acceptance).

Marked ``@pytest.mark.integration``; skip when no server reachable (conftest).
"""
import pytest

from lvsandbox_e2b import Sandbox
from lvsandbox_e2b.exceptions import SandboxException

pytestmark = pytest.mark.integration


def test_sandbox_create_and_kill(sandbox):
    # sandbox fixture already created+live; verify info then a fresh create/kill
    info = sandbox.get_info()
    assert info.sandbox_id == sandbox.sandbox_id
    assert info.state == "RUNNING"


def test_filesystem_roundtrip(sandbox):
    fs = sandbox.filesystem
    fs.write("/e2b_test.txt", b"hello")
    assert fs.read("/e2b_test.txt") == "hello"
    items = fs.list("/")
    assert any(f.name == "e2b_test.txt" and f.mode for f in items)
    assert fs.exists("/e2b_test.txt") is True
    fs.remove("/e2b_test.txt")
    assert fs.exists("/e2b_test.txt") is False


def test_commands_run_callbacks(sandbox):
    out = []
    p = sandbox.commands.run("echo hello", on_stdout=lambda b: out.append(b))
    assert b"hello" in b"".join(out)
    assert p.exit_code == 0


def test_commands_cwd_and_nonzero_exit(sandbox):
    sandbox.filesystem.make_dir("/e2b_sub")
    p = sandbox.commands.run("pwd", cwd="e2b_sub")
    assert p.exit_code == 0
    assert b"e2b_sub" in p.stdout
    p2 = sandbox.commands.run("exit 3")
    assert p2.exit_code == 3  # non-zero does not raise


def test_find_and_search(sandbox):
    fs = sandbox.filesystem
    fs.write("/e2b_find.py", b"TODO: fix me\nprint('x')\n")
    found = fs.find("/", "**/*.py")
    assert any(f.name == "e2b_find.py" for f in found)
    res = fs.search("/", "TODO")
    assert any(r.path == "e2b_find.py" for r in res)
    fs.remove("/e2b_find.py")


def test_snapshot_create_list_delete(sandbox):
    snap = sandbox.snapshot.create()
    snaps = sandbox.snapshot.list()
    assert any(s.snapshot_id == snap for s in snaps)
    sandbox.snapshot.delete(snap)


def test_killed_sandbox_raises_on_use():
    # fresh sandbox, kill, then use -> SandboxException
    if not __import__("conftest", fromlist=["_reachable"])._reachable():
        pytest.skip("no server")
    sb = Sandbox.create(template="base")
    sb.kill()
    with pytest.raises(SandboxException):
        sb.commands.run("ls")
