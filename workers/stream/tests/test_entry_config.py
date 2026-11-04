"""Configuration pins complement, not replace, generated package execution."""
import pathlib
import tomllib
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]


class EntryConfigTests(unittest.TestCase):
    def test_every_stream_config_uses_owned_entry_and_both_classes(self):
        paths = list(ROOT.glob('wrangler*.toml'))
        self.assertTrue(paths)
        self.assertFalse(list(ROOT.glob('wrangler*.json*')))
        for path in paths:
            with self.subTest(config=path.name):
                config = tomllib.loads(path.read_text())
                for environment in [config, *config.get('env', {}).values()]:
                    self.assertEqual(environment.get('main', config['main']), 'entry.mjs')
                    bindings = environment['durable_objects']['bindings']
                    self.assertEqual({b['class_name'] for b in bindings},
                                     {'NntpAccount', 'NntpPool'})
                    command = environment.get('build', config['build'])['command']
                    self.assertIn('worker-build --version 0.8.7 --locked', command)
                    self.assertIn('worker-build --release --locked', command)
                    self.assertIn('node --experimental-vm-modules tests/check-package.mjs', command)
                    self.assertIn('test -z "${CUSTOM_SHIM+x}${COREDUMP+x}"', command)
                    self.assertNotIn('--no-panic-recovery', command)
                    self.assertNotIn('--no-minify', command)

    def test_entry_preserves_sdk_exports_without_exporting_guard_helper(self):
        entry = (ROOT / 'entry.mjs').read_text()
        self.assertIn('guardDurableObjects(sdk);', entry)
        self.assertIn('export * from "./build/worker/shim.mjs";', entry)
        self.assertIn('export { default } from "./build/worker/shim.mjs";', entry)
        self.assertNotIn('export { guardDurableObjects', entry)
        runtime = (ROOT / 'src/account_runtime.rs').read_text()
        self.assertIn('const OPERATIONS_QUALIFIED: bool = false;', runtime)


if __name__ == '__main__':
    unittest.main()
