"""Integration checks for catalog pagination boundaries."""

import unittest

from catalog import CatalogItem, CatalogPage
from catalog_service import CatalogService

from tests.fakes import ScriptedCatalogTransport


class CatalogIntegrationTests(unittest.TestCase):
    """Ensure pagination preserves result and caller boundaries."""

    def test_overlapping_pages_are_deduplicated_in_first_seen_order(self) -> None:
        transport = ScriptedCatalogTransport(
            {
                1: CatalogPage(
                    (CatalogItem("A", "Desk"), CatalogItem("B", "Lamp")),
                    2,
                ),
                2: CatalogPage(
                    (CatalogItem("B", "Lamp"), CatalogItem("C", "Chair")),
                    3,
                ),
                3: CatalogPage(
                    (CatalogItem("C", "Chair"), CatalogItem("D", "Shelf")),
                    None,
                ),
            }
        )
        service = CatalogService(transport)
        filters = {"query": "office", "sort": "name"}

        items = service.list_items(filters)

        self.assertEqual(
            items,
            [
                CatalogItem("A", "Desk"),
                CatalogItem("B", "Lamp"),
                CatalogItem("C", "Chair"),
                CatalogItem("D", "Shelf"),
            ],
        )
        self.assertEqual(transport.calls, [1, 2, 3])

    def test_pagination_does_not_mutate_caller_filters(self) -> None:
        transport = ScriptedCatalogTransport(
            {
                1: CatalogPage((CatalogItem("A", "Desk"),), 2),
                2: CatalogPage((CatalogItem("B", "Lamp"),), None),
            }
        )
        service = CatalogService(transport)
        filters = {"query": "office", "sort": "name"}
        original_filters = dict(filters)

        service.list_items(filters)

        self.assertEqual(filters, original_filters)
        self.assertEqual(transport.filters_seen, [original_filters, original_filters])
