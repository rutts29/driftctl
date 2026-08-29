"""Integration checks for the user-import mutation boundary."""

import unittest

from import_users import import_users
from user_store import User, UserStore


class UserImportIntegrationTests(unittest.TestCase):
    """Ensure a failed import does not partially write users."""

    def test_valid_rows_are_imported_and_counted(self) -> None:
        store = UserStore()

        imported = import_users(
            "email,name\nalice@example.com,Alice\nbob@example.com,Bob\n",
            store,
        )

        self.assertEqual(imported, 2)
        self.assertEqual(
            store.all_users(),
            (
                User("alice@example.com", "Alice"),
                User("bob@example.com", "Bob"),
            ),
        )

    def test_malformed_later_row_rolls_back_prior_valid_rows(self) -> None:
        existing = User("existing@example.com", "Existing")
        store = UserStore((existing,))

        with self.assertRaises(ValueError):
            import_users(
                "email,name\nnew@example.com,New User\nincomplete@example.com\n",
                store,
            )

        self.assertEqual(store.all_users(), (existing,))

    def test_duplicate_later_row_rolls_back_prior_valid_rows(self) -> None:
        existing = User("existing@example.com", "Existing")
        store = UserStore((existing,))

        with self.assertRaises(ValueError):
            import_users(
                "email,name\nnew@example.com,New User\nexisting@example.com,Changed\n",
                store,
            )

        self.assertEqual(store.all_users(), (existing,))

    def test_duplicate_rows_within_one_import_are_rejected_atomically(self) -> None:
        store = UserStore()

        with self.assertRaises(ValueError):
            import_users(
                "email,name\nsame@example.com,First\nsame@example.com,Second\n",
                store,
            )

        self.assertEqual(store.all_users(), ())
