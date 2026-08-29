"""In-memory user store used by the admin import service."""

from dataclasses import dataclass
from typing import Iterable


@dataclass(frozen=True)
class User:
    """One user record accepted by the admin service."""

    email: str
    name: str


class UserStore:
    """Store users while enforcing unique email addresses."""

    def __init__(self, users: Iterable[User] = ()) -> None:
        self._users = list(users)

    def add(self, user: User) -> None:
        """Add a user unless its email is already present."""

        if any(existing.email == user.email for existing in self._users):
            raise ValueError(f"email already exists: {user.email}")
        self._users.append(user)

    def all_users(self) -> tuple[User, ...]:
        """Return users in insertion order."""

        return tuple(self._users)
