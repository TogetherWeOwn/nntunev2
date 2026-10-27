import { describe, expect, it } from 'vitest';
import { buildShareQuery } from './shareTimestamp.js';

/**
 * Test-Author owned RED acceptance for COPY-TIME: copy a demo-track link
 * at the current playback position. The Builder wires `/demo/player` to
 * satisfy this file plus `apps/web/tests/player-share-copy.spec.ts` and
 * must not edit either file.
 *
 * LOCKED CONTRACT (`apps/web/src/routes/demo/player/+page.svelte`):
 * - Declares a labeled native copy action `share-copy` ("Copy link at
 *   current time"), a polite status region `share-copy-status`, a readonly
 *   fallback URL `share-copy-fallback`, and actionable instructions
 *   `share-copy-instructions` (see the Playwright spec for behavior).
 * - Reuses `buildShareQuery` as the single encoding/timestamp source for
 *   the copied `track`/`t` query (floor + encodeURIComponent semantics).
 * - Never calls legacy `document.execCommand` (no automatic fallback).
 *
 * RED NOW: the route has no share-copy hooks and imports only
 * `parseTimestampParam` (inbound links). GREEN CONTROLS: the
 * `buildShareQuery` encoding pins and the no-execCommand pin pass on the
 * current tree, proving this file compiles and the helper contract holds.
 */

// Raw route markup via Vite glob: no node:fs, no @types/node needed.
const sources = import.meta.glob('../../routes/**/*.svelte', {
	query: '?raw',
	import: 'default',
	eager: true
}) as Record<string, string>;

const ROUTE = '../../routes/demo/player/+page.svelte';

function routeSource(): string {
	const source = sources[ROUTE];
	expect(
		source,
		`COPY-TIME: expected route source "${ROUTE}" to exist. NEXT: do not move the demo player route; Builder adds share-copy hooks there.`
	).toBeDefined();
	return source ?? '';
}

describe('share-copy hooks are declared in the demo player markup', () => {
	it('declares the labeled native copy action (share-copy)', () => {
		expect(
			routeSource(),
			'COPY-TIME: expected data-testid="share-copy" in the demo player markup (actual: missing). NEXT: Builder adds a native <button data-testid="share-copy">Copy link at current time</button> (see player-share-copy.spec.ts LOCKED CONTRACT).'
		).toContain('data-testid="share-copy"');
	});

	it('declares the polite status region (share-copy-status)', () => {
		expect(
			routeSource(),
			'COPY-TIME: expected data-testid="share-copy-status" in the demo player markup (actual: missing). NEXT: Builder adds the aria-live polite confirmation region.'
		).toContain('data-testid="share-copy-status"');
	});

	it('declares the readonly fallback URL (share-copy-fallback)', () => {
		expect(
			routeSource(),
			'COPY-TIME: expected data-testid="share-copy-fallback" in the demo player markup (actual: missing). NEXT: Builder adds the labeled readonly fallback input shown when the Clipboard API is absent or rejects.'
		).toContain('data-testid="share-copy-fallback"');
	});

	it('declares actionable copy instructions (share-copy-instructions)', () => {
		expect(
			routeSource(),
			'COPY-TIME: expected data-testid="share-copy-instructions" in the demo player markup (actual: missing). NEXT: Builder adds manual copy instructions shown with the fallback (no success claim).'
		).toContain('data-testid="share-copy-instructions"');
	});
});

describe('share-copy encoding source', () => {
	it('reuses buildShareQuery as the single encoding/timestamp source', () => {
		expect(
			routeSource(),
			'COPY-TIME: expected the demo player to reuse buildShareQuery (actual: no reference). NEXT: Builder imports buildShareQuery from #lib/player/shareTimestamp.js and builds the copied query with it (no cloned search/hash, no stream URL).'
		).toContain('buildShareQuery');
	});

	it('never uses a legacy execCommand fallback (control: passes now, must stay green)', () => {
		expect(
			routeSource(),
			'COPY-TIME: the demo player must never call document.execCommand (no legacy automatic fallback).'
		).not.toContain('execCommand');
	});

	it('copy query matches buildShareQuery floor + encode semantics (control)', () => {
		// Fractional floors, encoded ids, zero valid; invalid throws (RangeError).
		// The Playwright spec asserts copied URLs equal these shapes.
		expect(buildShareQuery('second-signal', 95.9)).toBe('track=second-signal&t=95');
		expect(buildShareQuery('demo-track', 0)).toBe('track=demo-track&t=0');
		expect(buildShareQuery('a b&c', 5)).toBe('track=a%20b%26c&t=5');
		expect(() => buildShareQuery('demo-track', Number.NaN)).toThrow(RangeError);
		expect(() => buildShareQuery('demo-track', -1)).toThrow(RangeError);
		expect(() => buildShareQuery('', 10)).toThrow(RangeError);
	});
});
