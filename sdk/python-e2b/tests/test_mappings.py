"""cr-083 S1: template->profile + HTTP->E2B error mapping."""
import pytest
from lvsandbox.errors import LvApiError

from lvsandbox_e2b._mappings import template_to_profile, translate_error
from lvsandbox_e2b.exceptions import (
    AuthenticationException,
    E2BError,
    FileNotFoundException,
    RateLimitException,
    SandboxException,
    TimeoutException,
)


def test_template_mapping():
    assert template_to_profile("base") == "shell"
    assert template_to_profile("python") == "python"
    assert template_to_profile("node") == "node"
    assert template_to_profile("custom-x") == "custom-x"  # 未知 -> 透传


def test_translate_404_session_vs_file():
    assert isinstance(
        translate_error(LvApiError(404, "session not found"), "session"),
        SandboxException,
    )
    assert isinstance(
        translate_error(LvApiError(404, "nope"), "file"), FileNotFoundException
    )


def test_translate_table():
    assert isinstance(
        translate_error(LvApiError(408, "timeout"), "session"), TimeoutException
    )
    assert isinstance(
        translate_error(LvApiError(401, "unauthorized"), "session"),
        AuthenticationException,
    )
    assert isinstance(
        translate_error(LvApiError(429, "slow down"), "session"), RateLimitException
    )
    assert isinstance(translate_error(LvApiError(500, "boom"), "session"), E2BError)


def test_translate_preserves_message():
    e = translate_error(LvApiError(408, "cmd timeout"), "command")
    assert "cmd timeout" in str(e)
