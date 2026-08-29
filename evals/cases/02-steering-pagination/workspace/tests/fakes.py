"""Deterministic catalog transport fakes for the fixture tests."""

from catalog import CatalogPage


class ScriptedCatalogTransport:
    """Return scripted pages while recording every request."""

    def __init__(self, pages: dict[int, CatalogPage]) -> None:
        self._pages = dict(pages)
        self.calls: list[int] = []
        self.filters_seen: list[dict[str, str]] = []

    def fetch(self, filters: dict[str, str], page: int) -> CatalogPage:
        """Record a request and return its scripted page."""

        self.calls.append(page)
        self.filters_seen.append(dict(filters))
        if page not in self._pages:
            raise AssertionError(f"the service requested an unscripted page: {page}")
        return self._pages[page]
