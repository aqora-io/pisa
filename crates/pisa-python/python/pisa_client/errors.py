"""Exceptions raised by :class:`pisa_client.Client`.

Server errors carry the API's error ``kind`` and the HTTP ``status``; the
class groups kinds by what the caller should do about them.
"""


class PisaError(Exception):
    """Base class. Also raised when the server can't be reached, in which
    case ``kind`` and ``status`` are None."""

    def __init__(self, message: str, kind: str | None = None, status: int | None = None):
        super().__init__(message)
        self.message = message
        self.kind = kind
        self.status = status


class LeanError(PisaError):
    """Lean or the policy refused the input (kinds ``lean`` and ``policy``),
    e.g. an unknown tactic. During proof search, an ordinary dead end."""


class LimitError(PisaError):
    """The input ran into a resource limit (kinds ``timeout``,
    ``out_of_memory`` and ``crash``)."""


class CapacityError(PisaError):
    """No worker could take the request, the metadata store failed, or a
    gateway in front of the server answered 502, 503 or 504 (kinds
    ``capacity`` and ``unavailable``); retry later. The client has already
    retried as far as its ``retry`` policy goes."""


class NotFoundError(PisaError):
    """Unknown or expired job, session or proof state, or an unknown
    snapshot or declaration."""


_CLASSES = {
    "lean": LeanError,
    "policy": LeanError,
    "timeout": LimitError,
    "out_of_memory": LimitError,
    "crash": LimitError,
    "capacity": CapacityError,
    "unavailable": CapacityError,
    "not_found": NotFoundError,
}


def _from_server(message: str, kind: str | None, status: int | None) -> PisaError:
    return _CLASSES.get(kind, PisaError)(message, kind, status)
