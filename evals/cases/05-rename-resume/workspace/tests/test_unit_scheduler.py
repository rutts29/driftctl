"""Unit checks for resumable failed work."""

import unittest

from scheduler import Scheduler
from work_queue import WorkQueue


class SchedulerUnitTests(unittest.TestCase):
    """Check the retry state transition before the later domain rename."""

    def test_resumes_failed_task_without_resetting_attempts(self) -> None:
        queue = WorkQueue()
        scheduler = Scheduler(queue)
        scheduler.submit("build-report")
        queue.mark_failed("build-report")

        item = scheduler.resume("build-report")

        self.assertEqual(item.task_id, "build-report")
        self.assertEqual(item.state, "pending")
        self.assertEqual(item.attempts, 1)
        self.assertEqual([pending.task_id for pending in queue.pending()], ["build-report"])
