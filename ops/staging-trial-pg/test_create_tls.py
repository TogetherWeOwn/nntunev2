"""Exercise TLS generation with disposable local output only."""

from pathlib import Path
import subprocess
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import patch

from create_tls import create_tls


class CreateTLS(unittest.TestCase):
    def test_failed_signing_leaves_no_partial_key_material(self):
        def openssl(argv, *, cwd, **kwargs):
            if argv[1] == "x509":
                raise subprocess.CalledProcessError(1, argv)
            (cwd / argv[argv.index("-keyout") + 1]).write_text("synthetic key")
            (cwd / argv[argv.index("-out") + 1]).write_text("synthetic certificate or request")

        with TemporaryDirectory() as scratch:
            output = Path(scratch) / "tls"
            with patch("create_tls.subprocess.run", side_effect=openssl):
                with self.assertRaises(subprocess.CalledProcessError):
                    create_tls(output, fixture=True)
            self.assertEqual(list(output.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
