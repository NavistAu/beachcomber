"""Exceptions raised by the beachcomber client."""


class CombError(Exception):
    """Base exception for all beachcomber client errors."""


class DaemonNotRunning(CombError):
    """Raised when the beachcomber daemon socket cannot be connected to.

    This usually means ``comb daemon`` is not running. Start it before
    using the client.
    """

    def __init__(self, socket_path: str) -> None:
        self.socket_path = socket_path
        super().__init__(
            f"beachcomber daemon is not running (tried {socket_path})"
        )


class ServerError(CombError):
    """Raised when the daemon returns ``ok: false`` in its response.

    Attributes:
        message: The error string from the daemon.
    """

    def __init__(self, message: str) -> None:
        self.message = message
        super().__init__(f"daemon error: {message}")


class ProtocolError(CombError):
    """Raised when a response cannot be parsed as valid JSON or is malformed."""
