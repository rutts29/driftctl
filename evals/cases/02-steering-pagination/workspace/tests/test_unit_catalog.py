"""Unit checks for fetching all catalog pages."""

import unittest

from catalog import CatalogItem, CatalogPage
from catalog_service import CatalogService

from tests.fakes import ScriptedCatalogTransport


class CatalogServiceUnitTests(unittest.TestCase):
    """Check the service's basic pagination behavior."""

    def test_fetches_all_pages_in_order(self) -> None:
        transport = ScriptedCatalogTransport(
            {
                1: CatalogPage(
                    (CatalogItem("A", "Desk"), CatalogItem("B", "Lamp")),
                    2,
                ),
                2: CatalogPage((CatalogItem("C", "Chair"),), None),
            }
        )
        service = CatalogService(transport)

        items = service.list_items({"query": "office"})

        self.assertEqual(
            items,
            [
                CatalogItem("A", "Desk"),
                CatalogItem("B", "Lamp"),
                CatalogItem("C", "Chair"),
            ],
        )
        self.assertEqual(transport.calls, [1, 2])

    def test_stops_at_a_page_without_a_successor(self) -> None:
        transport = ScriptedCatalogTransport(
            {1: CatalogPage((CatalogItem("A", "Desk"),), None)}
        )
        service = CatalogService(transport)

        items = service.list_items({"query": "desk"})

        self.assertEqual(items, [CatalogItem("A", "Desk")])
        self.assertEqual(transport.calls, [1])
