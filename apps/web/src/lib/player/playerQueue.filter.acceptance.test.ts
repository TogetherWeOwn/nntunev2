import { describe, expect, it } from 'vitest';
import { render } from 'svelte/server';
import PlayerQueue from './PlayerQueue.svelte';
import { createQueue, type Track } from './queue.js';

/**
 * QFILTER — independent test-only handoff; branch test/queue-filter-acceptance.
 * Protected paths: this file, tests/library-queue-filter.spec.ts and
 * tests/fixtures/queue-filter/* (no implementation or existing acceptance edits).
 *
 * Public UI contract: native input labeled "Filter queue", native button
 * "Clear queue filter", component-owned literal hook queue-filter-count with
 * "N of M tracks" and queue-filter-empty for a nonempty queue with no matches.
 * These are not route TESTIDS entries. Preserve all existing queue hooks.
 *
 * Run from repository root (Node 24 / pnpm 10):
 *   pnpm --dir apps/web check
 *   pnpm --dir apps/web test src/lib/player/playerQueue.filter.acceptance.test.ts
 *   pnpm --dir apps/web test:e2e tests/library-queue-filter.spec.ts --workers=1
 * Do not insert `--` after these pnpm scripts: this toolchain forwards it to
 * Vitest/Playwright, ending option parsing and selecting the entire suite.
 * Browser setup and the matching/index matrix live in the companion spec.
 * No speculative production helper import: pure matching vectors are exercised
 * through the real mounted component's public input, not a test-only matcher.
 * Expected pre-feature RED: missing labeled input, clear action and count.
 * Builder: consume this test bank in the combined green ready PR; do not open
 * a standalone ready PR with these deliberately RED tests or weaken assertions.
 */

const TRACKS: Track[] = [
	{ id: 'queue-coral', title: 'Coral Night', artist: 'Mira', streamUrl: '' },
	{ id: 'queue-pine', title: 'Pine Morning', artist: 'North', streamUrl: '' }
];

function html(tracks = TRACKS) {
	return render(PlayerQueue, { props: { state: createQueue(tracks) } }).body;
}

function hook(body: string, id: string): string {
	const match = new RegExp(`<([\\w-]+)\\b[^>]*data-testid="${id}"[^>]*>([\\s\\S]*?)<\\/\\1>`).exec(body);
	expect(match, `${id} presentation is missing`).not.toBeNull();
	return match?.[2].replace(/<[^>]*>/g, ' ').replace(/\s+/g, ' ').trim() ?? '';
}

describe('queue-only filter initial render', () => {
	it('QFILTER-U01 renders an editable native input for queue filtering', () => {
		// Browser role/name assertions own accessible labeling; do not prescribe
		// whether the component uses a wrapping label, for/id or aria-labelledby.
		const input = /<input\b[^>]*type="(?:text|search)"[^>]*>/i.exec(html())?.[0];
		expect(input, 'native queue-only filter input is missing').toBeDefined();
		expect(input).not.toMatch(/\sdisabled(?:[\s=>]|$)/);
	});

	it('QFILTER-U02 supplies a native clear-filter action separate from Clear queue', () => {
		const buttons = [...html().matchAll(/<button\b[^>]*>[\s\S]*?<\/button>/g)].map(([button]) => button);
		const clear = buttons.find((button) => /Clear queue filter/i.test(button));
		expect(clear, 'Clear queue filter is missing').toBeDefined();
		expect(clear).toMatch(/type="button"/);
		expect(buttons.some((button) => />\s*Clear queue\s*</.test(button))).toBe(true);
	});

	it('QFILTER-U03 initially counts every arranged row, without changing the selection summary', () => {
		const body = html();
		expect(hook(body, 'queue-filter-count')).toBe('2 of 2 tracks');
		expect(hook(body, 'queue-position')).toContain('Track 1 of 2');
		expect(hook(body, 'now-playing')).toContain('Coral Night');
	});

	it('QFILTER-U04 distinguishes an empty queue from a filtered nonempty queue', () => {
		const body = html([]);
		expect(hook(body, 'queue-filter-count')).toBe('0 of 0 tracks');
		expect(hook(body, 'now-playing')).toMatch(/queue is empty/i);
		expect(body).not.toMatch(/no matches/i);
	});

	it('QFILTER-U05 control: render preserves frozen state, original identity/order and selected row', () => {
		const value = { tracks: TRACKS.map((track) => Object.freeze({ ...track })), currentIndex: 1 };
		const before = JSON.stringify(value);
		Object.freeze(value.tracks);
		Object.freeze(value);
		const body = render(PlayerQueue, { props: { state: value } }).body;
		expect([...body.matchAll(/data-track-id="([^"]+)"/g)].map((match) => match[1])).toEqual(['queue-coral', 'queue-pine']);
		expect(body).toMatch(/data-track-id="queue-pine"[^>]*aria-current="true"/);
		expect(JSON.stringify(value)).toBe(before);
	});
});
