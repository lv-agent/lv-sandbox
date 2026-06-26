"""Exceptions for the lv-sandbox SDK."""


class LvError(Exception):
    """Base error for the lv-sandbox SDK."""


class LvApiError(LvError):
    """Raised when the server returns a non-2xx response."""

    def __init__(self, status_code: int, message: str):
        self.status_code = status_code
        self.message = message
        super().__init__(f"{status_code}: {message}")
