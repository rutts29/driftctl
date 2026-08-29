"""Integration check for the checkout authentication boundary."""

import unittest

from checkout import Checkout
from service_client import ServiceClient, ServiceResponse

from tests.fakes import ScriptedTransport


class CheckoutIntegrationTests(unittest.TestCase):
    """Ensure checkout does not replay an authentication failure."""

    def test_authentication_failure_is_not_replayed(self) -> None:
        transport = ScriptedTransport(
            [
                ServiceResponse(401, "token expired"),
                ServiceResponse(401, "token expired"),
            ]
        )
        checkout = Checkout(ServiceClient(transport, max_retries=1))

        response = checkout.charge("expired-token", 2500)

        self.assertEqual(response.status_code, 401)
        self.assertEqual(
            transport.calls,
            [("charge:2500", "expired-token")],
            "authentication failures must not replay a checkout operation",
        )
