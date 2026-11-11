import { describe, expect, it } from 'vitest';
import { buildShareQuery } from './shareTimestamp.js';

/**
 * Test-Author owned RED acceptance for COPY-TIME: copy a track link at the
 * current playback position. The product owner is `queueShare.ts`
 * (`createQueueShare`; the per-row "Copy link" action lives in
 * `PlayerQueue.svelte`, outcomes render in `+layout.svelte`). Formerly wired
 * through the retired `/demo/player` route; source pins now read the product
 * owners (EL ruling TWO-5795, one-time host/locator migration — encoding
 * vectors unchanged). The retired demo browser spec `player-share-copy.spec.ts`
 * covered the same at-current-time UI; this unit bank keeps the encoding core.
 *
 * LOCKED CONTRACT (product queue share):
 * - Labeled native copy action "Copy link" (per-row, `PlayerQueue.svelte`),
 *   a polite status region `queue-share-status`, a readonly fallback URL
 *   `queue-share-fallback`, and actionable instructions
 *   `queue-share-instructions` (`+layout.svelte` outcomes).
 * - Reuses `buildShareQuery` as the single encoding/timestamp source for
 *   the copied `track`/`t` query (floor + encodeURIComponent semantics).
 * - Never calls legacy `document.execCommand` (no automatic fallback).
 *
 * GREEN CONTROLS: the `buildShareQuery` encoding pins and the no-execCommand
 * pin pass on the current tree, proving this file compiles and the helper
 * contract holds.
 */

// Raw source via Vite glob: no node:fs, no @types/node needed. Product share
// owners only (EL TWO-5795 repoint from the retired `/demo/player` route).
const sources = import.meta.glob(['../../routes/+layout.svelte', './queueShare.ts', './PlayerQueue.svelte'], {
	query: '?raw',
	import: 'default',
	eager: true
}) as Record<string, string>;

function layoutSource(): string {
	const source = sources['../../routes/+layout.svelte'];
	expect(
		source,
		'COPY-TIME: expected product layout source "../../routes/+layout.svelte" to exist. NEXT: queue-share outcomes must render in the shell layout.'
	).toBeDefined();
	return source ?? '';
}

function shareSource(): string {
	const source = sources['./queueShare.ts'];
	expect(
		source,
		'COPY-TIME: expected product share owner "./queueShare.ts" to exist. NEXT: queue-share wiring must live in queueShare.ts.'
	).toBeDefined();
	return source ?? '';
}

function playerQueueSource(): string {
	const source = sources['./PlayerQueue.svelte'];
	expect(
		source,
		'COPY-TIME: expected product queue markup "./PlayerQueue.svelte" to exist. NEXT: per-row Copy link must live in PlayerQueue.svelte.'
	).toBeDefined();
	return source ?? '';
}

describe('share-copy hooks are declared in the product share markup', () => {
	it('declares the labeled native copy action (Copy link)', () => {
		expect(
			playerQueueSource(),
			'COPY-TIME: expected the labeled native "Copy link" action in the product queue markup (actual: missing). NEXT: PlayerQueue.svelte must expose a native per-row <button aria-label="Copy link …">Copy link</button> wired to queueShare.'
		).toContain('Copy link');
	});

	it('declares the polite status region (queue-share-status)', () => {
		expect(
			layoutSource(),
			'COPY-TIME: expected data-testid="queue-share-status" in the product layout markup (actual: missing). NEXT: +layout.svelte must expose the aria-live polite confirmation region.'
		).toContain('data-testid="queue-share-status"');
	});

	it('declares the readonly fallback URL (queue-share-fallback)', () => {
		expect(
			layoutSource(),
			'COPY-TIME: expected data-testid="queue-share-fallback" in the product layout markup (actual: missing). NEXT: +layout.svelte must expose the labeled readonly fallback input shown when the Clipboard API is absent or rejects.'
		).toContain('data-testid="queue-share-fallback"');
	});

	it('declares actionable copy instructions (queue-share-instructions)', () => {
		expect(
			layoutSource(),
			'COPY-TIME: expected data-testid="queue-share-instructions" in the product layout markup (actual: missing). NEXT: +layout.svelte must expose manual copy instructions shown with the fallback (no success claim).'
		).toContain('data-testid="queue-share-instructions"');
	});
});

describe('share-copy encoding source', () => {
	it('reuses buildShareQuery as the single encoding/timestamp source', () => {
		expect(
			shareSource(),
			'COPY-TIME: expected the product share owner to reuse buildShareQuery (actual: no reference). NEXT: queueShare.ts imports buildShareQuery from #lib/player/shareTimestamp.js and builds the copied query with it (no cloned search/hash, no stream URL).'
		).toContain('buildShareQuery');
	});

	it('never uses a legacy execCommand fallback (control: passes now, must stay green)', () => {
		expect(
			[layoutSource(), shareSource(), playerQueueSource()].join('\n'),
			'COPY-TIME: the product share owners must never call document.execCommand (no legacy automatic fallback).'
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
