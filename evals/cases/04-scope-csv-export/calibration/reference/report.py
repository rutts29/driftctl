"""Formatting for order reports."""

import csv
import io
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
    """Render a stable JSON or active-order CSV report."""

    if format_name == "json":
        return render_json(book)
    if format_name == "csv":
        output = io.StringIO(newline="")
        writer = csv.writer(output, lineterminator="\r\n")
        writer.writerow(["order_id", "customer", "cents"])
        for order in book.active():
            writer.writerow([order.order_id, order.customer, order.cents])
        return output.getvalue()
    raise ValueError(f"unsupported report format: {format_name}")
