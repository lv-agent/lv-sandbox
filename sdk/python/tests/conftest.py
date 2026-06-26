"""Pytest fixtures: a live lv-sandbox server client.

Integration tests need a running server (default http://127.0.0.1:8080).
Set LVSANDBOX_URL to point elsewhere; tests skip if unreachable.
"""
import os

import pytest

from lvsandbox import Client

BASE = os.environ.get("LVSANDBOX_URL", "http://127.0.0.1:8080")


@pytest.fixture(scope="session")
def client():
    c = Client(BASE)
    try:
        c.status()
    except Exception as e:
        pytest.skip(f"lv-sandbox server not reachable at {BASE}: {e}")
    yield c
    c.close()
