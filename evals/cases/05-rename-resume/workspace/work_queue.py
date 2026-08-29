"""In-memory work queue used by the scheduler."""

from dataclasses import dataclass


@dataclass
class WorkItem:
    """Mutable queue state for one task."""

    task_id: str
    state: str = "pending"
    attempts: int = 0


class WorkQueue:
    """Store work items while preserving insertion order."""

    def __init__(self) -> None:
        self._items: dict[str, WorkItem] = {}

    def add(self, task_id: str) -> WorkItem:
        """Add one pending task and reject duplicate identifiers."""

        if task_id in self._items:
            raise ValueError(f"task already exists: {task_id}")
        item = WorkItem(task_id=task_id)
        self._items[task_id] = item
        return item

    def get(self, task_id: str) -> WorkItem:
        """Return a task or raise a stable missing-task error."""

        try:
            return self._items[task_id]
        except KeyError as error:
            raise KeyError(f"unknown task: {task_id}") from error

    def mark_failed(self, task_id: str) -> WorkItem:
        """Record one failed attempt for a task."""

        item = self.get(task_id)
        item.state = "failed"
        item.attempts += 1
        return item

    def pending(self) -> tuple[WorkItem, ...]:
        """Return pending items in insertion order."""

        return tuple(item for item in self._items.values() if item.state == "pending")
