"""Small client used by the checkout service."""

from dataclasses import dataclass
from typing import Protocol


@dataclass(frozen=True)
class ServiceResponse:
    """The response returned by the payment service."""

    status_code: int
    body: str


class Transport(Protocol):
    """Minimal transport boundary used by the client and its tests."""

    def send(self, operation: str, token: str) -> ServiceResponse:
        """Send one operation to the remote service."""


class ServiceClient:
    """Send service requests.

    Retry behavior is intentionally the task under evaluation. The starting
    implementation makes one request so the existing checkout behavior stays
    easy to understand before the agent changes it.
    """

    def __init__(self, transport: Transport) -> None:
        self._transport = transport

    def send(self, operation: str, token: str) -> ServiceResponse:
        """Send one operation and return the service response."""

        return self._transport.send(operation, token)
