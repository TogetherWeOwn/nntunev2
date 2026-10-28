import { describe, expect, it } from 'vitest';
import {
	assertPatchedToolchain,
	assertWorkflowSelections,
	selectedToolchain
} from './toolchain-security.test-helper';

const declaration = 'env:\n  WRANGLER_VERSION: 4.153.0\n';
const envSelection = '${{ env.WRANGLER_VERSION }}';

function actionWorkflow(selection: string | null, env = declaration, before = '') {
	return `name: toolchain-fixture
on: workflow_dispatch
permissions:
  contents: read
${env}jobs:
  deploy:
    runs-on: ubuntu-latest
    steps:
${before}      - uses: cloudflare/wrangler-action@953926a2e2182532811c01a25e53647d93bf07c0
        with:
          apiToken: \${{ secrets.CLOUDFLARE_API_TOKEN }}
${selection === null ? '' : `          wranglerVersion: ${selection}\n`}          command: deploy --dry-run
`;
}

function cliWorkflow(script: string, env = declaration, style = '|') {
	return `name: toolchain-cli-fixture
on: workflow_dispatch
permissions:
  contents: read
${env}jobs:
  deploy:
    runs-on: ubuntu-latest
    steps:
      - name: Offline command fixture
        run: ${style}
${script.split('\n').map((line) => `          ${line}`).join('\n')}
`;
}

// Raw graph fixtures only: no alternate Undici release is installed or executed.
function graph(undici: string) {
	return `lockfileVersion: '9.0'
importers:
  .:
    devDependencies:
      '@sveltejs/adapter-cloudflare':
        specifier: ^8.0.1
        version: 8.0.1(@sveltejs/kit@3.1.0)
packages:
  '@sveltejs/adapter-cloudflare@8.0.1': {}
  wrangler@4.153.0: {}
  miniflare@5.20261023.0-beta: {}
  undici@${undici}: {}
snapshots:
  '@sveltejs/adapter-cloudflare@8.0.1(@sveltejs/kit@3.1.0)':
    dependencies:
      wrangler: 4.153.0
  wrangler@4.153.0:
    dependencies:
      miniflare: 5.20261023.0-beta
  miniflare@5.20261023.0-beta:
    dependencies:
      undici: ${undici}
  undici@${undici}: {}
`;
}

describe('toolchain guard: supported release line and exact selected graph', () => {
	it.each(['7.30.1', '7.30.2', '7.31.0'])(
		'accepts patched stable supported Undici %s',
		(version) => {
			const chain = selectedToolchain(graph(version));
			expect(chain).toEqual({
				wrangler: '4.153.0',
				miniflare: '5.20261023.0-beta',
				undici: version
			});
			expect(() => assertPatchedToolchain(chain)).not.toThrow();
		}
	);

	it.each([
		'7.29.1',
		'7.30.0',
		'7.30.1-beta.1',
		'7.31.0-rc.0',
		'7.30.1+unapproved',
		'7.030.1',
		'6.29.1',
		'8.12.1',
		'8.12.2',
		'9.0.0'
	])('rejects affected, unstable or unsupported Undici %s', (version) => {
		expect(() => assertPatchedToolchain(selectedToolchain(graph(version)))).toThrow();
	});

	it.each([['7.30.1', true], ['8.12.1', false]] as const)(
		'validates the selected bare release after resolving peer-qualified Undici %s',
		(version, allowed) => {
			const reference = `${version}(@types/node@22.0.0)`;
			const source = graph(version)
				.replace(`      undici: ${version}`, `      undici: ${reference}`)
				.replace(new RegExp(`  undici@${version.replaceAll('.', '\\.')}\\: \\{\\}\\n$`), `  undici@${reference}: {}\n`);
			const chain = selectedToolchain(source);
			expect(chain.undici).toBe(version);
			if (allowed) expect(() => assertPatchedToolchain(chain)).not.toThrow();
			else expect(() => assertPatchedToolchain(chain)).toThrow();
		}
	);

	it('does not accept an unreachable patched sibling instead of the selected affected node', () => {
		const source = graph('7.29.1')
			.replace('snapshots:\n', '  undici@7.30.1: {}\nsnapshots:\n')
			.concat('  undici@7.30.1: {}\n');
		expect(selectedToolchain(source).undici).toBe('7.29.1');
		expect(() => assertPatchedToolchain(selectedToolchain(source))).toThrow();
	});

	it.each([
		['missing selected edge', graph('7.30.1').replace('      undici: 7.30.1\n', '')],
		['missing selected package', graph('7.30.1').replace('  undici@7.30.1: {}\n', '')],
		['missing selected snapshot', graph('7.30.1').replace(/  undici@7\.30\.1: \{\}\n$/, '')],
		[
			'duplicate selected edge',
			graph('7.30.1').replace(
				'      undici: 7.30.1',
				'      undici: 7.29.1\n      undici: 7.30.1'
			)
		],
		['duplicate selected importer', graph('7.30.1').replace('  .:\n', '  .: {}\n  .:\n')]
	])('fails closed on %s', (_name, source) => {
		expect(() => selectedToolchain(source)).toThrow();
	});
});

describe('toolchain guard: executable action selections', () => {
	it.each([
		['canonical env wiring', actionWorkflow(envSelection)],
		['direct safe consumer without env token', actionWorkflow('4.153.0', '')],
		['direct safe consumer with safe env', actionWorkflow('4.153.0')],
		['quoted direct value', actionWorkflow('"4.153.0"', '')],
		['case-insensitive action input name', actionWorkflow('4.153.0', '').replace('wranglerVersion:', 'WRANGLERVERSION:')],
		['quoted key and value', actionWorkflow(envSelection, 'env:\n  "WRANGLER_VERSION": \'4.153.0\'\n')],
		['inline comment', actionWorkflow(envSelection, 'env:\n  WRANGLER_VERSION: 4.153.0 # security patch\n')],
		['quoted scalar then comment', actionWorkflow(envSelection, 'env:\n  WRANGLER_VERSION: "4.153.0" # security patch\n')],
		['flow env mapping', actionWorkflow(envSelection, 'env: { WRANGLER_VERSION: \'4.153.0\' }\n')],
		['block env scalar', actionWorkflow(envSelection, 'env:\n  WRANGLER_VERSION: >-\n    4.153.0\n')],
		['block action scalar', actionWorkflow('>-\n            ${{ env.WRANGLER_VERSION }}')],
		['literal action scalar with trailing newline', actionWorkflow('|\n            ${{ env.WRANGLER_VERSION }}')],
		['folded action scalar with trailing newline', actionWorkflow('>\n            ${{ env.WRANGLER_VERSION }}')],
		['CRLF', actionWorkflow(envSelection).replaceAll('\n', '\r\n')],
		['commented fake key', actionWorkflow(envSelection, '# WRANGLER_VERSION: 4.150.0\n' + declaration)],
		[
			'shell printf is not another mapping key',
			actionWorkflow(envSelection, declaration, '      - run: |\n          printf \'WRANGLER_VERSION: %s\\n\' "$WRANGLER_VERSION"\n')
		],
		[
			'printed package is not an executable consumer',
			actionWorkflow(envSelection, declaration, '      - run: |\n          printf \'wrangler@4.150.0\\n\'\n')
		]
	])('accepts %s', (_name, source) => {
		expect(assertWorkflowSelections(source, '4.153.0')).toBe(1);
	});

	it.each([
		['stale action input behind a safe env declaration', actionWorkflow('4.150.0')],
		['direct stale action without env token', actionWorkflow('4.150.0', '')],
		['missing action input', actionWorkflow(null)],
		['padded action input key is not the real input', actionWorkflow('4.153.0', '').replace('wranglerVersion:', '" wranglerVersion ":')],
		['padded env key is not the real declaration', actionWorkflow(envSelection, 'env:\n  " WRANGLER_VERSION ": 4.153.0\n')],
		['dynamic action input', actionWorkflow('${{ inputs.wrangler }}')],
		['unversioned action input', actionWorkflow('latest', '')],
		['missing env declaration', actionWorkflow(envSelection, '')],
		['unsafe env pin', actionWorkflow(envSelection, declaration.replace('4.153.0', '4.150.0'))],
		['comment inside quoted scalar is part of the value', actionWorkflow(envSelection, 'env:\n  WRANGLER_VERSION: "4.153.0 # not a comment"\n')],
		['genuine duplicate declaration', actionWorkflow(envSelection, declaration + '  WRANGLER_VERSION: 4.150.0\n')],
		['quoted duplicate declaration', actionWorkflow(envSelection, declaration + '  "WRANGLER_VERSION": 4.150.0\n')],
		['job env override', actionWorkflow(envSelection).replace('    runs-on:', '    env:\n      WRANGLER_VERSION: 4.150.0\n    runs-on:')],
		['step flow env override', actionWorkflow(envSelection).replace('        with:', '        env: { "WRANGLER_VERSION": "4.150.0" }\n        with:')],
		['duplicate action input', actionWorkflow(envSelection).replace('          command:', '          wranglerVersion: 4.150.0\n          command:')],
		['case-variant duplicate action input', actionWorkflow(envSelection).replace('          command:', '          WRANGLERVERSION: 4.150.0\n          command:')],
		['case-variant env override', actionWorkflow(envSelection).replace('    runs-on:', '    env:\n      wrangler_version: 4.150.0\n    runs-on:')],
		['dynamic job env map', actionWorkflow(envSelection).replace('    runs-on:', '    env: ${{ fromJSON(\'{"WRANGLER_VERSION":"4.150.0"}\') }}\n    runs-on:')],
		['dynamic step env map', actionWorkflow(envSelection).replace('        with:', '        env: ${{ fromJSON(\'{"WRANGLER_VERSION":"4.150.0"}\') }}\n        with:')],
		['aliased selection', actionWorkflow('*pin', 'env:\n  WRANGLER_VERSION: &pin 4.153.0\n')],
		['unscanned local composite action', actionWorkflow(null, '').replace('cloudflare/wrangler-action@953926a2e2182532811c01a25e53647d93bf07c0', './.github/actions/deploy')],
		['unsupported Wrangler container action', actionWorkflow(null, '').replace('cloudflare/wrangler-action@953926a2e2182532811c01a25e53647d93bf07c0', 'docker://cloudflare/wrangler:4.150.0')]
	])('rejects %s', (_name, source) => {
		expect(() => assertWorkflowSelections(source, '4.153.0')).toThrow();
	});
});

describe('toolchain guard: executable CLI selections and shell scalar text', () => {
	it.each([
		['direct safe CLI', cliWorkflow('npx --yes wrangler@4.153.0 deploy --dry-run', '')],
		['non-selection assignment prefix', cliWorkflow('NODE_ENV=production npx --yes wrangler@4.153.0 deploy --dry-run', '')],
		['conditional shell command', cliWorkflow('if true; then npx --yes wrangler@4.153.0 deploy --dry-run; fi', '')],
		['canonical env CLI', cliWorkflow('npx --yes wrangler@${{ env.WRANGLER_VERSION }} deploy --dry-run')],
		['quoted package token', cliWorkflow('npx -y "wrangler@4.153.0" deploy --dry-run', '')],
		['continuation and pipeline', cliWorkflow('npx --yes wrangler@${{env.WRANGLER_VERSION}} deployments list \\\n  | node -e \'console.log("wrangler deploy is only example text")\'')],
		['shell comment ignores old example', cliWorkflow('# npx wrangler@4.150.0 deploy\nnpx --yes wrangler@4.153.0 deploy', '')]
	])('accepts %s', (_name, source) => {
		expect(assertWorkflowSelections(source, '4.153.0')).toBe(1);
	});

	it.each(['|', '|-', '|+', '|2', '|2-', '>', '>-', '>2-'])(
		'handles shell text in YAML block scalar %s without counting fake keys',
		(style) => {
			const source = cliWorkflow('printf \'WRANGLER_VERSION: %s\\n\' "$WRANGLER_VERSION"', declaration, style);
			expect(assertWorkflowSelections(source, '4.153.0')).toBe(0);
		}
	);

	it.each([
		['stale CLI behind safe declaration', cliWorkflow('npx --yes wrangler@4.150.0 deploy')],
		['direct stale CLI without env token', cliWorkflow('npx --yes wrangler@4.150.0 deploy', '')],
		['unversioned npx selection', cliWorkflow('npx --yes wrangler deploy', '')],
		['padded env value changes an unquoted CLI package token', cliWorkflow('npx --yes wrangler@${{ env.WRANGLER_VERSION }} deploy', 'env:\n  WRANGLER_VERSION: " 4.153.0 "\n')],
		['dynamic CLI selection', cliWorkflow('npx --yes wrangler@${{ inputs.version }} deploy')],
		['shell-variable CLI selection', cliWorkflow('npx --yes wrangler@$WRANGLER_VERSION deploy')],
		['bare installed CLI', cliWorkflow('wrangler deploy')],
		['other package runner', cliWorkflow('pnpm dlx wrangler@4.150.0 deploy')],
		['npm package override', cliWorkflow('npm exec --package=wrangler@4.150.0 -- wrangler deploy')],
		['npx package override', cliWorkflow('npx --package=wrangler@4.150.0 wrangler deploy')],
		['nested shell consumer', cliWorkflow('sh -c "npx wrangler@4.150.0 deploy"')],
		['command substitution', cliWorkflow('printf "%s" "$(npx wrangler@4.150.0 --version)"')],
		['backtick substitution', cliWorkflow('echo `npx wrangler@4.150.0 --version`')],
		['interpreter exec wrapper', cliWorkflow('node -e "require(\'child_process\').execSync(\'npx wrangler@4.150.0 deploy\')"', '')],
		['Python exec wrapper', cliWorkflow('python3 -c "import os; os.system(\'npx wrangler@4.150.0 deploy\')"', '')],
		['shell selection assignment', cliWorkflow('WRANGLER_VERSION=4.150.0 npx --yes wrangler@${{ env.WRANGLER_VERSION }} deploy')],
		['here-document substitution', cliWorkflow('cat <<EOF\n$(npx wrangler@4.150.0 --version)\nEOF', '')]
	])('rejects unsupported or unsafe %s', (_name, source) => {
		expect(() => assertWorkflowSelections(source, '4.153.0')).toThrow();
	});

	it('does not treat an echo argument or a comment as an executable selection', () => {
		const source = cliWorkflow('echo "npx wrangler@4.150.0 deploy"\n# wrangler@4.150.0', '');
		expect(assertWorkflowSelections(source, '4.153.0')).toBe(0);
	});

	it.each(["'EOF'", 'EOF', '"EOF"'])(
		'keeps literal here-document text with delimiter %s out of executable selection discovery',
		(delimiter) => {
			const source = cliWorkflow(`cat <<${delimiter}\nnpx wrangler@4.150.0 deploy\nWRANGLER_VERSION: 4.150.0\nEOF`, '');
			expect(assertWorkflowSelections(source, '4.153.0')).toBe(0);
		}
	);
});
