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
    """Send service requests with one transient-failure retry."""

    def __init__(self, transport: Transport, max_retries: int = 0) -> None:
        self._transport = transport
        self._max_retries = max_retries

    def send(self, operation: str, token: str) -> ServiceResponse:
        """Retry one server failure without replaying authorization failures."""

        response = self._transport.send(operation, token)
        if response.status_code >= 500 and self._max_retries > 0:
            return self._transport.send(operation, token)
        return response
