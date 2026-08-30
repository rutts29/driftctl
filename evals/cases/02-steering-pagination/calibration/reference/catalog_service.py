"""Application service for reading catalog search results."""

from catalog import CatalogItem, CatalogTransport


class CatalogService:
    """Return every catalog item while retaining its first occurrence."""

    def __init__(self, transport: CatalogTransport) -> None:
        self._transport = transport

    def list_items(self, filters: dict[str, str]) -> list[CatalogItem]:
        """Fetch all pages without changing the caller's filters."""

        items: list[CatalogItem] = []
        seen_ids: set[str] = set()
        page_number = 1
        while True:
            page = self._transport.fetch(dict(filters), page_number)
            for item in page.items:
                if item.item_id not in seen_ids:
                    seen_ids.add(item.item_id)
                    items.append(item)
            if page.next_page is None:
                return items
            page_number = page.next_page
