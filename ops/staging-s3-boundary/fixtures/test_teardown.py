"""Exercise disposable fixture teardown with simulated Docker subprocesses."""

from pathlib import Path
from subprocess import CompletedProcess
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import patch

import qualify


class TeardownTest(unittest.TestCase):
    def test_close_checks_project_inventory_after_successful_down(self):
        for volume_remains in (False, True):
            with self.subTest(volume_remains=volume_remains), TemporaryDirectory() as scratch:
                stack = qualify.Stack(Path(scratch), recording=False)
                self.assertEqual(stack.cmd[stack.cmd.index("-p") + 1], stack.project)
                label = f"label=com.docker.compose.project={stack.project}"
                commands = [
                    [*stack.cmd, "down", "--volumes", "--remove-orphans"],
                    [*qualify.DOCKER, "container", "ls", "-q", "--all", "--filter", label],
                    [*qualify.DOCKER, "network", "ls", "-q", "--filter", label],
                    [*qualify.DOCKER, "volume", "ls", "-q", "--filter", label],
                ]
                volume = f"{stack.project}_fixture-data\n".encode() if volume_remains else b""
                results = [
                    CompletedProcess(command, 0, stdout, b"")
                    for command, stdout in zip(commands, (b"", b"", b"", volume), strict=True)
                ]

                with patch.object(qualify.subprocess, "run", side_effect=results) as docker:
                    if volume_remains:
                        with self.assertRaisesRegex(RuntimeError, "^disposable volume remains$"):
                            stack.close()
                    else:
                        stack.close()

                self.assertEqual([invocation.args[0] for invocation in docker.call_args_list], commands)
