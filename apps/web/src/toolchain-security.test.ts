import { describe, expect, it } from 'vitest';
import lockfile from '../pnpm-lock.yaml?raw';
import {
	assertPatchedToolchain,
	assertWorkflowSelections,
	selectedToolchain
} from './toolchain-security.test-helper';

// Keep the literal call: Vitest's retained-cache glob exemption does not match
// a TypeScript generic between import.meta.glob and its opening parenthesis.
const workflows = import.meta.glob('../../../.github/workflows/*.{yml,yaml}', {
	query: '?raw',
	import: 'default',
	eager: true
}) as Record<string, string>;
const entries = Object.entries(workflows).map(([path, yaml]) => [path.replace(/^.*\//, ''), yaml] as const);

describe('Cloudflare toolchain security (offline)', () => {
	it('resolves the web adapter through stable undici >=7.30.1', () => {
		assertPatchedToolchain(selectedToolchain(lockfile));
	});

	it('discovers workflow YAML and executable Wrangler consumers instead of a fixed deployment list', () => {
		expect(entries.length, 'workflow YAML discovery must not be empty').toBeGreaterThan(0);
		const consumers = entries.reduce(
			(count, [, yaml]) => count + assertWorkflowSelections(yaml, selectedToolchain(lockfile).wrangler),
			0
		);
		expect(consumers, 'executable Wrangler discovery must not be empty').toBeGreaterThan(0);
	});

	it.each(entries)('%s keeps executable Wrangler selections patched', (_workflow, yaml) => {
		assertWorkflowSelections(yaml, selectedToolchain(lockfile).wrangler);
	});
});
