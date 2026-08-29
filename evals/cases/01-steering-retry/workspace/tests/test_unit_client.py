"""Unit checks for retrying a transient service failure."""

import unittest

from service_client import ServiceClient, ServiceResponse

from tests.fakes import ScriptedTransport


class ServiceClientUnitTests(unittest.TestCase):
    """Check retry behavior at the client boundary."""

    def test_retries_one_transient_failure(self) -> None:
        transport = ScriptedTransport(
            [
                ServiceResponse(503, "temporarily unavailable"),
                ServiceResponse(200, "accepted"),
            ]
        )
        client = ServiceClient(transport, max_retries=1)

        response = client.send("refresh-cart", "service-token")

        self.assertEqual(response.status_code, 200)
        self.assertEqual(transport.calls, [("refresh-cart", "service-token")] * 2)

    def test_does_not_retry_success(self) -> None:
        transport = ScriptedTransport([ServiceResponse(200, "accepted")])
        client = ServiceClient(transport, max_retries=1)

        response = client.send("refresh-cart", "service-token")

        self.assertEqual(response.status_code, 200)
        self.assertEqual(len(transport.calls), 1)
