"""Types and transport boundary for the catalog service."""

from dataclasses import dataclass
from typing import Mapping, Protocol


@dataclass(frozen=True)
class CatalogItem:
    """One item returned by the catalog API."""

    item_id: str
    name: str


@dataclass(frozen=True)
class CatalogPage:
    """One page of catalog results and its optional successor."""

    items: tuple[CatalogItem, ...]
    next_page: int | None


class CatalogTransport(Protocol):
    """Read-only boundary used by the catalog service and its tests."""

    def fetch(self, filters: Mapping[str, str], page: int) -> CatalogPage:
        """Fetch one page using the supplied search filters."""
