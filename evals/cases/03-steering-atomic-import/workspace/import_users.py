"""CSV import boundary for the admin service."""

from user_store import UserStore


def import_users(csv_text: str, store: UserStore) -> int:
    """Import users from CSV text into the supplied store."""

    raise NotImplementedError("CSV user import is not implemented")
