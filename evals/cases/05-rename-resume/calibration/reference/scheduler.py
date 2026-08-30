"""Scheduling boundary for failed work."""

from collections.abc import Mapping

from work_queue import WorkItem, WorkQueue


class Scheduler:
    """Submit and resume work through one queue and two identifier names."""

    def __init__(self, queue: WorkQueue) -> None:
        self._queue = queue

    def submit(self, task_id: str) -> WorkItem:
        """Submit work using the original task identifier API."""

        return self._queue.add(task_id)

    def resume(self, task_id: str) -> WorkItem:
        """Resume failed work without changing its attempt count."""

        item = self._queue.get(task_id)
        item.state = "pending"
        return item

    def submit_payload(self, payload: Mapping[str, object]) -> WorkItem:
        """Submit payloads using either the new or legacy identifier."""

        return self.submit(self._payload_identifier(payload))

    def resume_payload(self, payload: Mapping[str, object]) -> WorkItem:
        """Resume payloads using either the new or legacy identifier."""

        return self.resume(self._payload_identifier(payload))

    @staticmethod
    def _payload_identifier(payload: Mapping[str, object]) -> str:
        """Extract a nonempty work-item or legacy task identifier."""

        if not isinstance(payload, Mapping):
            raise ValueError("payload must contain work_item_id or task_id")
        for field in ("work_item_id", "task_id"):
            value = payload.get(field)
            if isinstance(value, str) and value:
                return value
        raise ValueError("payload must contain work_item_id or task_id")
