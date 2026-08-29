"""Order records and stable collection behavior."""

from dataclasses import dataclass


@dataclass(frozen=True)
class Order:
    """One order that can appear in a customer report."""

    order_id: str
    customer: str
    cents: int
    status: str = "open"


class OrderBook:
    """Keep the source order while exposing the active report view."""

    def __init__(self, orders: list[Order]) -> None:
        self._orders = tuple(orders)

    def active(self) -> tuple[Order, ...]:
        """Return open orders in their original order."""

        return tuple(order for order in self._orders if order.status == "open")

    def all(self) -> tuple[Order, ...]:
        """Return every stored order in its original order."""

        return self._orders
