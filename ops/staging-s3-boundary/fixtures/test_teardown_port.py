"""Exercise the disposable fixture port check without opening a socket."""

from contextlib import nullcontext
from pathlib import Path
from subprocess import CompletedProcess
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import patch

import qualify


class TeardownPortTest(unittest.TestCase):
    def test_close_requires_connection_refusal_on_prior_port(self):
        prior_port = 49152
        for accepting in (True, False):
            with self.subTest(accepting=accepting), TemporaryDirectory() as scratch:
                stack = qualify.Stack(Path(scratch), recording=False)
                stack.port = prior_port
                label = f"label=com.docker.compose.project={stack.project}"
                commands = [
                    [*stack.cmd, "down", "--volumes", "--remove-orphans"],
                    [*qualify.DOCKER, "container", "ls", "-q", "--all", "--filter", label],
                    [*qualify.DOCKER, "network", "ls", "-q", "--filter", label],
                    [*qualify.DOCKER, "volume", "ls", "-q", "--filter", label],
                ]
                results = [CompletedProcess(command, 0, b"", b"") for command in commands]

                with (
                    patch.object(qualify.subprocess, "run", side_effect=results) as docker,
                    patch.object(qualify.socket, "create_connection", return_value=nullcontext(),
                                 side_effect=None if accepting else ConnectionRefusedError) as connect,
                ):
                    if accepting:
                        with self.assertRaisesRegex(RuntimeError, "^disposable loopback port is still open$"):
                            stack.close()
                    else:
                        stack.close()

                self.assertEqual([invocation.args[0] for invocation in docker.call_args_list], commands)
                connect.assert_called_once()
                self.assertEqual(connect.call_args.args, (("127.0.0.1", prior_port),))
