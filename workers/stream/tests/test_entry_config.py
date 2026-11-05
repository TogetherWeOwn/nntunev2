"""Configuration pins complement, not replace, generated package execution."""
import pathlib
import tomllib
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]


class EntryConfigTests(unittest.TestCase):
    def test_every_stream_config_keeps_account_deployment_uninstalled(self):
        paths = list(ROOT.glob('wrangler*.toml'))
        self.assertTrue(paths)
        self.assertFalse(list(ROOT.glob('wrangler*.json*')))
        for path in paths:
            with self.subTest(config=path.name):
                config = tomllib.loads(path.read_text())
                for environment in [config, *config.get('env', {}).values()]:
                    self.assertEqual(environment.get('main', config['main']), 'entry.mjs')
                    bindings = environment['durable_objects']['bindings']
                    self.assertEqual({(b['name'], b['class_name']) for b in bindings},
                                     {('NNTP_POOL', 'NntpPool')})
                    self.assertEqual(environment.get('migrations', config['migrations']),
                                     [{'tag': 'v1', 'new_sqlite_classes': ['NntpPool']}])
                    command = environment.get('build', config['build'])['command']
                    self.assertIn('worker-build --version 0.8.7 --locked', command)
                    self.assertIn('worker-build --release --locked', command)
                    self.assertIn('node --experimental-vm-modules tests/check-package.mjs', command)
                    self.assertIn('test -z "${CUSTOM_SHIM+x}${COREDUMP+x}"', command)
                    self.assertNotIn('--no-panic-recovery', command)
                    self.assertNotIn('--no-minify', command)

    def test_entry_is_only_the_existing_sdk_reexport(self):
        entry = (ROOT / 'entry.mjs').read_text()
        code = '\n'.join(line for line in entry.splitlines()
                         if line.strip() and not line.lstrip().startswith('//'))
        self.assertEqual(code, 'export * from "./build/worker/shim.mjs";\n'
                               'export { default } from "./build/worker/shim.mjs";')

    def test_account_runtime_and_existing_do_forwarding_are_uninstalled(self):
        library = (ROOT / 'src/lib.rs').read_text()
        self.assertNotIn('pub mod account_runtime;', library)
        runtime = (ROOT / 'src/runtime.rs').read_text()
        self.assertNotIn('crate::account_runtime', runtime)
        self.assertNotIn('crate::account_request', runtime)
        self.assertIn('routes::decide_pool_action(req.method().as_ref(), &req.path(), has_owner_name)',
                      runtime)
        account = (ROOT / 'src/account_runtime.rs').read_text()
        self.assertIn('const OPERATIONS_QUALIFIED: bool = false;', account)


if __name__ == '__main__':
    unittest.main()
