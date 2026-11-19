"""Signal-free tests of bounded owned process-group cleanup."""

import signal
import unittest
from unittest.mock import Mock, call, patch

import process_group


class ProcessGroupTests(unittest.TestCase):
    def setUp(self):
        self.process = Mock(pid=12345)
        self.clock = 0.0
        self.signals = []
        self.probes = []

    def sleep(self, seconds):
        self.clock += seconds

    def killpg(self, pgid, sig):
        self.assertEqual(pgid, self.process.pid)
        self.signals.append(sig)
        if sig == 0 and not self.probes.pop(0):
            raise ProcessLookupError

    def shutdown(self, probes, **kwargs):
        self.probes = list(probes)
        with (
            patch.object(process_group.os, "getpgrp", return_value=99),
            patch.object(process_group.os, "killpg", side_effect=self.killpg),
            patch.object(
                process_group.time, "monotonic", side_effect=lambda: self.clock
            ),
            patch.object(process_group.time, "sleep", side_effect=self.sleep),
        ):
            process_group.shutdown_process_group(self.process, **kwargs)

    def test_leader_already_exited_still_signals_group(self):
        self.process.poll.return_value = 0
        self.shutdown([False])
        self.assertEqual(self.signals, [signal.SIGTERM, 0])
        self.process.poll.assert_called()

    def test_successful_termination_does_not_escalate(self):
        self.shutdown([True, False], term_timeout=1)
        self.assertEqual(self.signals, [signal.SIGTERM, 0, 0])
        self.assertGreater(self.clock, 0)

    def test_surviving_descendant_is_killed_and_disappearance_checked(self):
        self.process.poll.return_value = 0
        self.shutdown([True, False], term_timeout=0, kill_timeout=1)
        self.assertEqual(self.signals, [signal.SIGTERM, 0, signal.SIGKILL, 0])

    def test_persistent_group_raises_after_both_deadlines(self):
        with self.assertRaises(TimeoutError):
            self.shutdown([True, True], term_timeout=0, kill_timeout=0)
        self.assertEqual(self.signals, [signal.SIGTERM, 0, signal.SIGKILL, 0])

    def test_already_absent_group_is_success(self):
        with (
            patch.object(process_group.os, "getpgrp", return_value=99),
            patch.object(
                process_group.os, "killpg", side_effect=ProcessLookupError
            ) as kill,
        ):
            process_group.shutdown_process_group(self.process)
        kill.assert_has_calls([call(self.process.pid, signal.SIGTERM)])
        self.process.poll.assert_called_once()

    def test_os_errors_propagate(self):
        with (
            patch.object(process_group.os, "getpgrp", return_value=99),
            patch.object(process_group.os, "killpg", side_effect=PermissionError),
            self.assertRaises(PermissionError),
        ):
            process_group.shutdown_process_group(self.process)

    def test_rejects_caller_group(self):
        with (
            patch.object(process_group.os, "getpgrp", return_value=self.process.pid),
            patch.object(process_group.os, "killpg") as kill,
            self.assertRaises(ValueError),
        ):
            process_group.shutdown_process_group(self.process)
        kill.assert_not_called()

    def test_rejects_invalid_timeouts_before_signalling(self):
        with (
            patch.object(process_group.os, "getpgrp", return_value=99),
            patch.object(process_group.os, "killpg") as kill,
        ):
            for parameter in ("term_timeout", "kill_timeout"):
                for value in (-1, float("inf"), float("nan")):
                    with self.subTest(parameter=parameter, value=value):
                        with self.assertRaises(ValueError):
                            process_group.shutdown_process_group(
                                self.process, **{parameter: value}
                            )
                        kill.assert_not_called()


if __name__ == "__main__":
    unittest.main()
