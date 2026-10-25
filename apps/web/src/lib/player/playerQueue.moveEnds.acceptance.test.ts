import { describe, expect, it } from 'vitest';
import { render } from 'svelte/server';
import PlayerQueue from './PlayerQueue.svelte';
import { createQueue, type QueueState, type Track } from './queue.js';

/**
 * QTOP acceptance-owned contract; Builder must not edit this file.
 * Test bank ref: test/queue-move-ends-acceptance.
 *
 * Public contract: every PlayerQueue row gains "Move to top" / "Move to
 * bottom" buttons with per-track accessible names `Move {title} to top`
 * and `Move {title} to bottom` (extending the existing `Move {title} up`
 * / `Move {title} down` names). Buttons whose end is unreachable — top on
 * the first row, bottom on the last row, both on a single-track queue —
 * are disabled or hidden. Existing row actions (Up/Down/Next/Remove), the
 * host-rendered `queue-announcement` region and the -1-or-absent row
 * tabindex contract stay untouched. Clicks, order changes and announcement
 * text are pinned in tests/queue-move-ends.spec.ts (QTOP-B-*); the pure
 * helpers are pinned in queue.moveEnds.acceptance.test.ts (QTOP-U-*).
 *
 * Reproduce from apps/web:
 *   pnpm install --frozen-lockfile
 *   pnpm test src/lib/player/playerQueue.moveEnds.acceptance.test.ts
 * Expected pre-feature RED: assertion failures for the missing top/bottom
 * buttons, not import/compile errors. The Builder consumes these tests in
 * one combined ready PR; never merge the RED test bank by itself.
 *
 * Passing disabled-or-absent end checks are negative controls, not feature
 * evidence: the present-and-enabled assertions below are the RED proof.
 */

function track(id: string, title: string): Track {
	return { id, title, artist: 'Move Ends Band', streamUrl: `/test-audio/${id}`, artworkUrl: null };
}

const MANGO = track('moveends-mango', 'Mango Season');
const APPLE = track('moveends-apple', 'Apple Cart');
const ZEBRA = track('moveends-zebra', 'Zebra Crossing');

function state(tracks: Track[], currentIndex: number): QueueState {
	return { ...createQueue(tracks), currentIndex };
}

function renderQueue(value: QueueState): string {
	return render(PlayerQueue, { props: { state: value } }).body;
}

interface RowBlock {
	id: string;
	html: string;
}

/** Split the SSR list into one block per queue row, keyed by track id. */
function rowBlocks(body: string): RowBlock[] {
	const blocks: RowBlock[] = [];
	const pattern = /<li\b[^>]*data-testid="queue-item"[^>]*>([\s\S]*?)<\/li>/g;
	for (let match = pattern.exec(body); match; match = pattern.exec(body)) {
		const id = match[0].match(/data-track-id="([^"]+)"/)?.[1] ?? '';
		blocks.push({ id, html: match[0] });
	}
	return blocks;
}

function rowById(body: string, id: string): string {
	const row = rowBlocks(body).find((block) => block.id === id);
	expect(row, `queue row for track ${id} must render`).toBeDefined();
	return row!.html;
}

type ButtonState = 'enabled' | 'disabled' | 'absent';

/** Locate a row button by its exact accessible name. */
function buttonState(rowHtml: string, label: string): ButtonState {
	const escaped = label.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
	const pattern = new RegExp(`<button\\b[^>]*aria-label="${escaped}"[^>]*>`);
	const tag = rowHtml.match(pattern)?.[0];
	if (!tag) return 'absent';
	return /\bdisabled\b/.test(tag) ? 'disabled' : 'enabled';
}

function topLabel(title: string): string {
	return `Move ${title} to top`;
}

function bottomLabel(title: string): string {
	return `Move ${title} to bottom`;
}

describe('queue panel move-to-end buttons (acceptance)', () => {
	it('QTOP-P-01 exposes both end actions, enabled, on a middle row', () => {
		const row = rowById(renderQueue(state([MANGO, APPLE, ZEBRA], 0)), APPLE.id);

		expect(buttonState(row, topLabel(APPLE.title))).toBe('enabled');
		expect(buttonState(row, bottomLabel(APPLE.title))).toBe('enabled');
	});

	it('QTOP-P-02 disables or hides top on the first row while bottom stays enabled', () => {
		const row = rowById(renderQueue(state([MANGO, APPLE, ZEBRA], 0)), MANGO.id);

		expect(['disabled', 'absent']).toContain(buttonState(row, topLabel(MANGO.title)));
		expect(buttonState(row, bottomLabel(MANGO.title))).toBe('enabled');
	});

	it('QTOP-P-03 disables or hides bottom on the last row while top stays enabled', () => {
		const row = rowById(renderQueue(state([MANGO, APPLE, ZEBRA], 0)), ZEBRA.id);

		expect(buttonState(row, topLabel(ZEBRA.title))).toBe('enabled');
		expect(['disabled', 'absent']).toContain(buttonState(row, bottomLabel(ZEBRA.title)));
	});

	it('QTOP-P-04 disables or hides both end actions on a single-track queue', () => {
		const row = rowById(renderQueue(state([MANGO], 0)), MANGO.id);

		expect(['disabled', 'absent']).toContain(buttonState(row, topLabel(MANGO.title)));
		expect(['disabled', 'absent']).toContain(buttonState(row, bottomLabel(MANGO.title)));
	});

	it('QTOP-P-05 keeps the existing per-row actions on every row (regression guard)', () => {
		const body = renderQueue(state([MANGO, APPLE, ZEBRA], 0));

		for (const item of [MANGO, APPLE, ZEBRA]) {
			const row = rowById(body, item.id);
			expect(buttonState(row, `Move ${item.title} up`)).not.toBe('absent');
			expect(buttonState(row, `Move ${item.title} down`)).not.toBe('absent');
			expect(buttonState(row, `Play ${item.title} next`)).toBe('enabled');
			expect(buttonState(row, `Remove ${item.title} from queue`)).toBe('enabled');
		}
	});

	it('QTOP-P-06 keeps the announcement region and adds no row tab stops (regression guard)', () => {
		const body = renderQueue(state([MANGO, APPLE, ZEBRA], 0));

		expect(body).toContain('data-testid="queue-announcement"');
		for (const tag of body.matchAll(/<[a-zA-Z][^>]*data-testid="queue-item"[^>]*>/g)) {
			const tabindex = tag[0].match(/tabindex="([^"]*)"/)?.[1];
			expect(tabindex === undefined || tabindex === '-1').toBe(true);
		}
		expect(renderQueue(createQueue([]))).not.toContain('to top');
		expect(renderQueue(createQueue([]))).not.toContain('to bottom');
	});
});
