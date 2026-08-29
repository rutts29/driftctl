"""Scheduling boundary for failed work."""

from work_queue import WorkItem, WorkQueue


class Scheduler:
    """Submit tasks and resume failed work through one queue."""

    def __init__(self, queue: WorkQueue) -> None:
        self._queue = queue

    def submit(self, task_id: str) -> WorkItem:
        """Submit a task using the original task identifier API."""

        return self._queue.add(task_id)

    def resume(self, task_id: str) -> WorkItem:
        """Resume one failed task without changing its attempt count."""

        raise NotImplementedError("resuming failed tasks is not implemented")
