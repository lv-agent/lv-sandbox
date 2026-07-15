"""Handles + error translation (cr-083).

`translating(context)` 把 lvsandbox 抛出的 ``LvApiError`` 翻译成对应的 E2B
异常;shim 的每个公开方法用它包一层 server 调用。
"""
from contextlib import asynccontextmanager, contextmanager

from lvsandbox.errors import LvApiError

from ._mappings import translate_error


@contextmanager
def translating(context: str):
    """Re-raise ``LvApiError`` as the matching E2B exception (sync)."""
    try:
        yield
    except LvApiError as e:
        raise translate_error(e, context) from e


@asynccontextmanager
async def translating_async(context: str):
    """Async counterpart of :func:`translating` (for AsyncSandbox)."""
    try:
        yield
    except LvApiError as e:
        raise translate_error(e, context) from e
