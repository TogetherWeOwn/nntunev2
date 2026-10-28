import { describe, expect, it } from 'vitest';
import lockfile from '../pnpm-lock.yaml?raw';

const workflows = import.meta.glob<string>('../../../.github/workflows/*.{yml,yaml}', {
	query: '?raw',
	import: 'default',
	eager: true
});
const wranglerWorkflows = Object.entries(workflows)
	.map(([path, yaml]) => [path.replace(/^.*\//, ''), yaml.replace(/^\s*#.*$/gm, '')] as const)
	.filter(([, yaml]) => /\bWRANGLER_VERSION\b/.test(yaml));

// Read pnpm's block mappings at their exact scope, not a safe version elsewhere
// in the file. Missing or duplicate edges fail closed without a YAML dependency.
function entry(source: string, key: string, indent: number) {
	const lines = source.split('\n');
	const matches = lines.flatMap((line, index) => {
		const match = line.match(/^( *)(?:'([^']+)'|"([^"]+)"|([^ :][^:]*)):/);
		return match?.[1]?.length === indent && (match[2] ?? match[3] ?? match[4]) === key
			? [index]
			: [];
	});
	expect(matches, `expected one mapping entry for ${key}`).toHaveLength(1);
	const start = matches[0]!;
	let end = start + 1;
	while (
		end < lines.length &&
		(!lines[end]!.trim() ||
			lines[end]!.trimStart().startsWith('#') ||
			lines[end]!.startsWith(' '.repeat(indent + 1)))
	) {
		end++;
	}
	return {
		value: lines[start]!
			.slice(lines[start]!.indexOf(':') + 1)
			.trim()
			.replace(/^(['"])(.*)\1$/, '$2'),
		body: lines.slice(start + 1, end).join('\n')
	};
}

function selectedToolchain() {
	const importers = entry(lockfile, 'importers', 0).body;
	const importer = entry(importers, '.', 2).body;
	const devDependencies = entry(importer, 'devDependencies', 4).body;
	const adapter = entry(devDependencies, '@sveltejs/adapter-cloudflare', 6).body;
	const adapterVersion = entry(adapter, 'version', 8).value;
	const packages = entry(lockfile, 'packages', 0).body;
	const snapshots = entry(lockfile, 'snapshots', 0).body;

	function snapshot(name: string, reference: string) {
		entry(packages, `${name}@${reference.split('(')[0]}`, 2);
		return entry(snapshots, `${name}@${reference}`, 2).body;
	}
	function dependency(name: string, reference: string, child: string) {
		const dependencies = entry(snapshot(name, reference), 'dependencies', 4).body;
		return entry(dependencies, child, 6).value;
	}

	const wrangler = dependency('@sveltejs/adapter-cloudflare', adapterVersion, 'wrangler');
	const miniflare = dependency('wrangler', wrangler, 'miniflare');
	const undici = dependency('miniflare', miniflare, 'undici');
	snapshot('undici', undici);
	return { wrangler: wrangler.split('(')[0], miniflare, undici };
}

describe('Cloudflare toolchain security (offline)', () => {
	it('resolves the web adapter through stable undici >=7.30.1', () => {
		const { wrangler, miniflare, undici } = selectedToolchain();
		expect(undici, 'selected undici must be a stable release').toMatch(/^\d+\.\d+\.\d+$/);
		const [major = 0, minor = 0, patch = 0] = undici.split('.').map(Number);
		expect(
			major > 7 || (major === 7 && (minor > 30 || (minor === 30 && patch >= 1))),
			`Wrangler ${wrangler} -> Miniflare ${miniflare} -> undici ${undici}: requires undici >=7.30.1`
		).toBe(true);
	});

	it('discovers workflow YAML and Wrangler selections instead of a fixed deployment list', () => {
		expect(Object.keys(workflows).length, 'workflow YAML discovery must not be empty').toBeGreaterThan(0);
		expect(wranglerWorkflows.length, 'Wrangler workflow discovery must not be empty').toBeGreaterThan(0);
	});

	it.each(wranglerWorkflows)(
		'%s deploys with the patched Wrangler selected by the web adapter',
		(workflow, yaml) => {
			// Keep the supported selection shape explicit: a single workflow-level
			// literal. Reject narrower/flow overrides rather than ignore their pins.
			expect(
				yaml.match(/\bWRANGLER_VERSION['"]?\s*:/g) ?? [],
				`${workflow} must define WRANGLER_VERSION only once, at workflow scope`
			).toHaveLength(1);
			expect(yaml, `${workflow} must not use an explicit YAML selection key`).not.toMatch(
				/^\s*\?\s*['"]?WRANGLER_VERSION\b/m
			);
			const env = entry(yaml, 'env', 0).body;
			const pin = entry(env, 'WRANGLER_VERSION', 2).value;
			expect(pin, `${workflow} must pin patched Wrangler`).toBe('4.153.0');
			expect(pin, `${workflow} must match the resolved web toolchain`).toBe(
				selectedToolchain().wrangler
			);
		}
	);
});
