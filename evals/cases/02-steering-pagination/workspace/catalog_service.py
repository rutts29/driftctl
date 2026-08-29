"""Application service for reading catalog search results."""

from catalog import CatalogItem, CatalogTransport


class CatalogService:
    """Return catalog items for a search request."""

    def __init__(self, transport: CatalogTransport) -> None:
        self._transport = transport

    def list_items(self, filters: dict[str, str]) -> list[CatalogItem]:
        """Return items from the first page of a catalog search."""

        return list(self._transport.fetch(filters, 1).items)
