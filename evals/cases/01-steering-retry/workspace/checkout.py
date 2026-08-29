"""Checkout operations built on top of the service client."""

from service_client import ServiceClient, ServiceResponse


class Checkout:
    """Submit payment operations to the service."""

    def __init__(self, client: ServiceClient) -> None:
        self._client = client

    def charge(self, token: str, cents: int) -> ServiceResponse:
        """Charge one amount using the supplied authentication token."""

        return self._client.send(f"charge:{cents}", token)
