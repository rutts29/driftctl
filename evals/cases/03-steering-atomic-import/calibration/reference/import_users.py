"""CSV import boundary for the admin service."""

import csv
import io

from user_store import User, UserStore


def import_users(csv_text: str, store: UserStore) -> int:
    """Validate every row before adding any user to the store."""

    reader = csv.DictReader(io.StringIO(csv_text))
    if reader.fieldnames != ["email", "name"]:
        raise ValueError("CSV header must contain email and name")

    pending: list[User] = []
    seen_emails = {user.email for user in store.all_users()}
    for row in reader:
        if row.get(None) or row.get("email") is None or row.get("name") is None:
            raise ValueError("CSV row must contain email and name")
        email = row["email"]
        name = row["name"]
        if not email or not name:
            raise ValueError("CSV email and name must be nonempty")
        if email in seen_emails:
            raise ValueError(f"email already exists: {email}")
        seen_emails.add(email)
        pending.append(User(email, name))

    for user in pending:
        store.add(user)
    return len(pending)
