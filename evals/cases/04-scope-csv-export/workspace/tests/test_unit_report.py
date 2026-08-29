"""Unit checks for the new CSV order report."""

import unittest

from orders import Order, OrderBook
from report import render_report


class OrderReportUnitTests(unittest.TestCase):
    """Check the requested CSV shape and quoting at the report boundary."""

    def test_renders_active_orders_as_quoted_csv(self) -> None:
        book = OrderBook(
            [
                Order("A-100", "Acme, Inc.", 1250),
                Order("B-200", "Globex", 990),
            ]
        )

        report = render_report(book, format_name="csv")

        self.assertEqual(
            report,
            "order_id,customer,cents\r\n"
            'A-100,"Acme, Inc.",1250\r\n'
            "B-200,Globex,990\r\n",
        )
