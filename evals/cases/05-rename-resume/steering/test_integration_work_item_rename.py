"""Integration checks after the task domain was renamed."""

import unittest

from scheduler import Scheduler
from work_queue import WorkQueue


class WorkItemRenameIntegrationTests(unittest.TestCase):
    """Ensure both identifier names preserve the resumed-work contract."""

    def test_accepts_work_item_and_legacy_task_payloads(self) -> None:
        queue = WorkQueue()
        scheduler = Scheduler(queue)

        scheduler.submit_payload({"work_item_id": "invoice-7"})
        queue.mark_failed("invoice-7")
        resumed = scheduler.resume_payload({"work_item_id": "invoice-7"})

        self.assertEqual(resumed.task_id, "invoice-7")
        self.assertEqual(resumed.state, "pending")
        self.assertEqual(resumed.attempts, 1)

        legacy = scheduler.submit_payload({"task_id": "legacy-3"})
        queue.mark_failed("legacy-3")
        legacy_resumed = scheduler.resume_payload({"task_id": "legacy-3"})

        self.assertEqual(legacy.task_id, "legacy-3")
        self.assertEqual(legacy_resumed.state, "pending")
        self.assertEqual(legacy_resumed.attempts, 1)

    def test_rejects_payload_without_an_identifier(self) -> None:
        scheduler = Scheduler(WorkQueue())

        with self.assertRaises(ValueError):
            scheduler.submit_payload({"label": "missing id"})
