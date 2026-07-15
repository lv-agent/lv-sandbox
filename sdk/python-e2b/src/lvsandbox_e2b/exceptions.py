"""E2B-aligned exception hierarchy (cr-083 §6.1).

类名与 E2B Python SDK 对齐;用户 `from lvsandbox_e2b import ...` 捕获的异常
与官方 `from e2b import ...` 同名同层级。
"""


class E2BError(Exception):
    """Base error for lvsandbox-e2b."""


class SandboxException(E2BError):
    """Sandbox lifecycle error."""


class TimeoutException(SandboxException):
    """Operation timed out."""


class SandboxNotRunningError(SandboxException):
    """Sandbox is not running."""


class FileException(E2BError):
    """Filesystem error."""


class FileNotFoundException(FileException):
    """Path does not exist."""


class PermissionDeniedError(FileException):
    """Permission denied."""


class CommandException(E2BError):
    """Command execution error."""


class CommandExitException(CommandException):
    """Command exited non-zero.

    Note: ``commands.run`` does NOT raise this on non-zero exit (per §6.4);
    it only sets ``process.exit_code``. This class is exposed for parity with
    the E2B surface and future explicit-raise callers.
    """


class TemplateException(E2BError):
    """Template operation unsupported (v1 raises NotImplementedError)."""


class PTYException(E2BError):
    """PTY error (reserved; pty not in v1 scope)."""


class AuthenticationException(E2BError):
    """Authentication failed."""


class RateLimitException(E2BError):
    """Rate limited (HTTP 429)."""
