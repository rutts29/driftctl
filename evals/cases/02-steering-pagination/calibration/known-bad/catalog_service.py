"""Application service for reading catalog search results."""

from catalog import CatalogItem, CatalogTransport


class CatalogService:
    """Return every catalog item without resolving overlapping pages."""

    def __init__(self, transport: CatalogTransport) -> None:
        self._transport = transport

    def list_items(self, filters: dict[str, str]) -> list[CatalogItem]:
        """Fetch all pages while leaving duplicate results in place."""

        items: list[CatalogItem] = []
        page_number = 1
        while True:
            page = self._transport.fetch(dict(filters), page_number)
            items.extend(page.items)
            if page.next_page is None:
                return items
            page_number = page.next_page
