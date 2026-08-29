"""Integration check for the checkout authentication boundary."""

import unittest

from checkout import Checkout
from service_client import ServiceClient, ServiceResponse


class ScriptedTransport:
    """Return evaluator-owned responses while recording each operation."""

    def __init__(self, responses: list[ServiceResponse]) -> None:
        self._responses = list(responses)
        self.calls: list[tuple[str, str]] = []

    def send(self, operation: str, token: str) -> ServiceResponse:
        self.calls.append((operation, token))
        if not self._responses:
            raise AssertionError("the client made an unexpected extra request")
        return self._responses.pop(0)


class CheckoutIntegrationTests(unittest.TestCase):
    """Ensure checkout does not replay an authentication failure."""

    def test_transient_failure_is_retried_once(self) -> None:
        transport = ScriptedTransport(
            [
                ServiceResponse(503, "temporarily unavailable"),
                ServiceResponse(200, "accepted"),
            ]
        )
        checkout = Checkout(ServiceClient(transport, max_retries=1))

        response = checkout.charge("valid-token", 2500)

        self.assertEqual(response.status_code, 200)
        self.assertEqual(
            transport.calls,
            [
                ("charge:2500", "valid-token"),
                ("charge:2500", "valid-token"),
            ],
        )

    def test_authentication_failure_is_not_replayed(self) -> None:
        for status_code in (401, 403):
            with self.subTest(status_code=status_code):
                transport = ScriptedTransport(
                    [
                        ServiceResponse(status_code, "authorization rejected"),
                        ServiceResponse(status_code, "authorization rejected"),
                    ]
                )
                checkout = Checkout(ServiceClient(transport, max_retries=1))

                response = checkout.charge("rejected-token", 2500)

                self.assertEqual(response.status_code, status_code)
                self.assertEqual(
                    transport.calls,
                    [("charge:2500", "rejected-token")],
                    "authentication failures must not replay a checkout operation",
                )
