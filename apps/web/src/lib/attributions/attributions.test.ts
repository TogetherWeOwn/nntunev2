import { describe, expect, it } from 'vitest';
import packageJson from '../../../package.json';
import lockfile from '../../../pnpm-lock.yaml?raw';
import {
	attributionInventory, activeAttributions, plannedAttributions,
	fixtureAudioLicenceRecord, validateAttributionInventory, WEB_TOOL_PACKAGES
} from './index.js';

const clone = () => JSON.parse(JSON.stringify(attributionInventory));

// All direct declarations must be classified even though this package currently
// lists its runtime frameworks in devDependencies. Transitives are out of scope.
function declarations(manifest: Record<string, unknown>): Record<string, string> {
	return Object.assign({}, ...['dependencies', 'devDependencies', 'optionalDependencies', 'peerDependencies']
		.map((key) => manifest[key] ?? {}));
}

function inventoryDrift(manifest: Record<string, unknown>, lock: string): string[] {
	const declared = declarations(manifest);
	const packages = attributionInventory.entries.flatMap((entry) => entry.package ? [entry.package] : []);
	const classified = [...WEB_TOOL_PACKAGES, ...packages.map((pkg) => pkg.name)].sort();
	const errors: string[] = [];
	if (JSON.stringify(Object.keys(declared).sort()) !== JSON.stringify(classified)) errors.push('classification drift');
	// Parse only the pinned pnpm v9 root importer layout, never the transitive
	// packages section. An unsupported layout fails closed (missing records).
	const importer = lock.split('\npackages:')[0];
	const locked = new Map([...importer.matchAll(/^      '?([^'\n]+?)'?:\n        specifier: ([^\n]+)\n        version: ([^\n(]+)/gm)]
		.map((match) => [match[1], { specifier: match[2], version: match[3] }]));
	for (const pkg of packages) {
		const resolved = locked.get(pkg.name);
		if (declared[pkg.name] !== pkg.specifier || resolved?.specifier !== pkg.specifier || resolved?.version !== pkg.version) {
			errors.push(`package drift: ${pkg.name}`);
		}
	}
	return errors;
}

describe('attribution inventory v1', () => {
	it('validates the serializable inventory and exports only active records for current use', () => {
		expect(validateAttributionInventory(clone())).toEqual([]);
		expect(activeAttributions.length).toBeGreaterThan(0);
		expect(activeAttributions.every((entry) => entry.status === 'active')).toBe(true);
		expect(plannedAttributions.map((entry) => entry.id)).toEqual(['musicbrainz-core', 'cover-art', 'listenbrainz', 'lrclib']);
		expect(activeAttributions.some((entry) => ['musicbrainz-core', 'cover-art', 'listenbrainz', 'lrclib'].includes(entry.id))).toBe(false);
		expect(fixtureAudioLicenceRecord).toBe('docs/fixture-audio-licences.md');
	});

	it.each(['id', 'name', 'kind', 'status', 'license', 'sourceUrl', 'licenseUrl', 'notice', 'obligations', 'evidence', 'package'])(
		'rejects absent required field %s', (field) => {
			const value = clone();
			delete value.entries[0][field];
			expect(validateAttributionInventory(value).length).toBeGreaterThan(0);
		}
	);

	it.each([null, [], {}, { schemaVersion: 2, entries: [] }, { schemaVersion: 1, entries: [null] }])(
		'rejects malformed roots and entries: %j', (value) => {
			expect(validateAttributionInventory(value).length).toBeGreaterThan(0);
		}
	);

	it.each([
		{ kind: ['package'] },
		{ kind: { toString: (): string => 'package' } }
	])('rejects coerced package kinds without package metadata: %j', ({ kind }) => {
		const value = clone();
		value.entries[0].kind = kind;
		value.entries[0].status = 'planned';
		delete value.entries[0].package;
		expect(validateAttributionInventory(value)).toContain('entries[0].kind is unknown');
	});

	it('rejects empty fields, unknown states, malformed package records and duplicate IDs', () => {
		for (const [field, bad] of [
			['name', '  '], ['notice', false], ['status', 'enabled'], ['kind', 'vendor'],
			['evidence', []], ['evidence', ['']], ['package', {}]
		] as const) {
			const value = clone();
			value.entries[0][field] = bad;
			expect(validateAttributionInventory(value).length, field).toBeGreaterThan(0);
		}
		const duplicate = clone();
		duplicate.entries.push(duplicate.entries[0]);
		expect(validateAttributionInventory(duplicate)).toContain('entries[9].id is duplicated');
	});

	it.each(['', 'not a URL', '/relative', 'http://example.org', 'javascript:alert(1)', 'https://', 'https://user:pass@example.org', ' https://example.org'])(
		'rejects invalid provenance URL %s', (bad) => {
			for (const field of ['sourceUrl', 'licenseUrl']) {
				const value = clone();
				value.entries[0][field] = bad;
				expect(validateAttributionInventory(value)).toContain(`entries[0].${field} must be an HTTPS URL`);
			}
		}
	);

	it('pins direct package classification, declarations and locked runtime versions', () => {
		expect(inventoryDrift(packageJson, lockfile)).toEqual([]);
		const changed = JSON.parse(JSON.stringify(packageJson));
		changed.dependencies = { 'new-runtime': '^1.0.0' };
		expect(inventoryDrift(changed, lockfile)).toContain('classification drift');
		changed.devDependencies.svelte = '^6.0.0';
		expect(inventoryDrift(changed, lockfile)).toContain('package drift: svelte');
		delete changed.devDependencies['@sveltejs/kit'];
		expect(inventoryDrift(changed, lockfile)).toContain('package drift: @sveltejs/kit');
		expect(inventoryDrift(packageJson, lockfile.replace('version: 5.57.3\n', 'version: 5.57.4\n')))
			.toContain('package drift: svelte');
		expect(inventoryDrift(packageJson, '')).toHaveLength(3);
	});
});
