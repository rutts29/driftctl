"""Unit checks for importing valid users from CSV."""

import unittest

from import_users import import_users
from user_store import User, UserStore


class UserImportUnitTests(unittest.TestCase):
    """Check the basic valid-import contract."""

    def test_imports_rows_and_returns_count(self) -> None:
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

    def test_empty_import_changes_nothing(self) -> None:
        store = UserStore((User("existing@example.com", "Existing"),))

        imported = import_users("email,name\n", store)

        self.assertEqual(imported, 0)
        self.assertEqual(
            store.all_users(),
            (User("existing@example.com", "Existing"),),
        )
