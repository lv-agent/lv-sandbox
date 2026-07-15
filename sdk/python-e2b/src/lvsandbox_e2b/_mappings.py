"""template -> profile + HTTP -> E2B error mapping (cr-083 §5, §6.4)."""
from lvsandbox.errors import LvApiError

from .exceptions import (
    AuthenticationException,
    E2BError,
    FileNotFoundException,
    RateLimitException,
    SandboxException,
    TimeoutException,
)

# E2B template name -> lv-sandbox profile name. Unknown -> passthrough
# (assume profile name == template name; server rejects unknown profiles).
_TEMPLATE_PROFILE = {"base": "shell", "python": "python", "node": "node"}


def template_to_profile(template: str) -> str:
    """Map an E2B template id to an lv-sandbox profile name."""
    return _TEMPLATE_PROFILE.get(template, template)


def translate_error(err: LvApiError, context: str) -> E2BError:
    """Map a ``LvApiError`` to the appropriate E2B exception.

    context: ``"session"`` | ``"file"`` | ``"command"`` —— disambiguates 404
    (session-not-found vs file-not-found).
    """
    code = err.status_code
    if code == 404:
        if context == "file":
            return FileNotFoundException(str(err))
        return SandboxException(str(err))
    if code == 408:
        return TimeoutException(str(err))
    if code == 401:
        return AuthenticationException(str(err))
    if code == 429:
        return RateLimitException(str(err))
    return E2BError(str(err))
