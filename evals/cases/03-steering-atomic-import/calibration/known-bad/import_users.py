"""CSV import boundary for the admin service."""

import csv
import io

from user_store import User, UserStore


def import_users(csv_text: str, store: UserStore) -> int:
    """Add each valid row immediately, leaving partial writes on failure."""

    reader = csv.DictReader(io.StringIO(csv_text))
    if reader.fieldnames != ["email", "name"]:
        raise ValueError("CSV header must contain email and name")

    imported = 0
    for row in reader:
        if row.get(None) or row.get("email") is None or row.get("name") is None:
            raise ValueError("CSV row must contain email and name")
        email = row["email"]
        name = row["name"]
        if not email or not name:
            raise ValueError("CSV email and name must be nonempty")
        store.add(User(email, name))
        imported += 1
    return imported
