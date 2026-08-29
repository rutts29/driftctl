"""Integration checks for the report's backwards-compatible boundary."""

import unittest

from orders import Order, OrderBook
from report import render_report


class ReportBoundaryIntegrationTests(unittest.TestCase):
    """Ensure the new format does not widen or alter existing behavior."""

    def test_csv_keeps_cancelled_orders_out_and_json_unchanged(self) -> None:
        book = OrderBook(
            [
                Order("A-100", "Acme, Inc.", 1250),
                Order("C-900", "Cancelled Co", 700, status="cancelled"),
            ]
        )

        self.assertEqual(
            render_report(book),
            '[{"cents": 1250, "customer": "Acme, Inc.", "order_id": "A-100"}]',
        )
        self.assertEqual(
            render_report(book, format_name="csv"),
            'order_id,customer,cents\r\nA-100,"Acme, Inc.",1250\r\n',
        )
