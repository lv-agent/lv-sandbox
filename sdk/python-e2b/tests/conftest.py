"""Integration fixtures: a live lv-sandbox server.

Integration tests need a running server (default http://127.0.0.1:8080). Set
LVSANDBOX_URL to point elsewhere; the suite skips if unreachable.
"""
import os

import pytest

from lvsandbox_e2b import Sandbox

BASE = os.environ.get("LVSANDBOX_URL", "http://127.0.0.1:8080")


def _reachable() -> bool:
    try:
        Sandbox.list(base_url=BASE)
        return True
    except Exception:
        return False


@pytest.fixture(scope="session")
def sandbox():
    if not _reachable():
        pytest.skip(f"lv-sandbox server not reachable at {BASE}")
    sb = Sandbox.create(template="base", base_url=BASE)
    yield sb
    try:
        sb.kill()
    except Exception:
        pass
