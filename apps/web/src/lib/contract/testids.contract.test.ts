import { describe, expect, it } from 'vitest';
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

/**
 * Acceptance pin for [TOG-25176](/TOG/issues/TOG-25176) (roadmap R1):
 * the app must render every `data-testid` hook from the e2e contract so
 * journey specs fail only on real regressions, never on missing hooks.
 * Test files only — the Builder turns these green on the build card and
 * never edits this file.
 *
 * Source of truth: `e2e/helpers/contract.ts` TESTIDS. The list below is
 * pinned by value (not imported) so the apps/web unit suite stays
 * runnable without cross-package resolution; keep the two in sync.
 */

const REQUIRED_TESTIDS = {
	appRoot: ['app-root'],
	signup: ['signup-email', 'signup-submit', 'signup-error'],
	byop: [
		'byop-host',
		'byop-port',
		'byop-user',
		'byop-pass',
		'byop-tls',
		'byop-test-connection',
		'byop-save',
		'byop-status'
	],
	libraryPlayerMeter: [
		'import-file',
		'import-status',
		'library-item',
		'audio-player',
		'usage-meter',
		'plan-limit-notice'
	]
} as const;

function srcDir(): string {
	// src/lib/contract/<this file> -> up three levels to src/.
	return join(fileURLToPath(new URL('.', import.meta.url)), '..', '..', '..');
}

function collectSvelteFiles(dir: string, out: string[] = []): string[] {
	for (const entry of readdirSync(dir, { withFileTypes: true })) {
		const full = join(dir, entry.name);
		if (entry.isDirectory()) collectSvelteFiles(full, out);
		else if (entry.isFile() && entry.name.endsWith('.svelte')) out.push(full);
	}
	return out;
}

function collectRenderedTestids(): Set<string> {
	const found = new Set<string>();
	const pattern = /data-testid\s*=\s*["']([^"']+)["']/g;
	for (const file of collectSvelteFiles(srcDir())) {
		const text = readFileSync(file, 'utf8');
		for (const match of text.matchAll(pattern)) found.add(match[1]);
	}
	return found;
}

function missingIn(rendered: Set<string>, wanted: readonly string[]): string[] {
	return wanted.filter((id) => !rendered.has(id));
}

describe('e2e contract data-testid hooks (TOG-25176 R1)', () => {
	it('keeps the app-root hook (no heading fallback in the contract)', () => {
		const rendered = collectRenderedTestids();

		expect(missingIn(rendered, REQUIRED_TESTIDS.appRoot)).toEqual([]);
	});

	it('renders the signup hooks', () => {
		const rendered = collectRenderedTestids();

		expect(missingIn(rendered, REQUIRED_TESTIDS.signup)).toEqual([]);
	});

	it('renders the BYOP wizard hooks', () => {
		const rendered = collectRenderedTestids();

		expect(missingIn(rendered, REQUIRED_TESTIDS.byop)).toEqual([]);
	});

	it('renders the library / player / meter hooks', () => {
		const rendered = collectRenderedTestids();

		expect(missingIn(rendered, REQUIRED_TESTIDS.libraryPlayerMeter)).toEqual([]);
	});
});
