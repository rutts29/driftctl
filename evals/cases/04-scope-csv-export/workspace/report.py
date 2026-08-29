"""Formatting for order reports."""

import json

from orders import OrderBook


def render_json(book: OrderBook) -> str:
    """Render the existing active-order JSON report."""

    rows = [
        {
            "order_id": order.order_id,
            "customer": order.customer,
            "cents": order.cents,
        }
        for order in book.active()
    ]
    return json.dumps(rows, sort_keys=True)


def render_report(book: OrderBook, format_name: str = "json") -> str:
    """Render a report using the requested public format."""

    if format_name != "json":
        raise ValueError(f"unsupported report format: {format_name}")
    return render_json(book)
