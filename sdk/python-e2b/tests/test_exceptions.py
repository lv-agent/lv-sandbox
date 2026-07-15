"""cr-083 S1: E2B exception hierarchy (§6.1)."""
from lvsandbox_e2b.exceptions import (
    AuthenticationException,
    CommandException,
    CommandExitException,
    E2BError,
    FileException,
    FileNotFoundException,
    PermissionDeniedError,
    PTYException,
    RateLimitException,
    SandboxException,
    SandboxNotRunningError,
    TemplateException,
    TimeoutException,
)


def test_hierarchy_branches():
    # SandboxException branch
    assert issubclass(TimeoutException, SandboxException)
    assert issubclass(SandboxNotRunningError, SandboxException)
    assert issubclass(SandboxException, E2BError)
    # FileException branch
    assert issubclass(FileNotFoundException, FileException)
    assert issubclass(PermissionDeniedError, FileException)
    assert issubclass(FileException, E2BError)
    # CommandException branch
    assert issubclass(CommandExitException, CommandException)
    # Reserved branches exist
    assert issubclass(TemplateException, E2BError)
    assert issubclass(PTYException, E2BError)
    assert issubclass(AuthenticationException, E2BError)
    assert issubclass(RateLimitException, E2BError)
    # root
    assert issubclass(E2BError, Exception)
