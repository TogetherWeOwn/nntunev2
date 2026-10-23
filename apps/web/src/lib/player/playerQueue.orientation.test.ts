import { describe, expect, it } from 'vitest';
import { render } from 'svelte/server';
import PlayerQueue from './PlayerQueue.svelte';
import { createQueue, type QueueState, type Track } from './queue.js';

/**
 * QUEUE-ORIENTATION acceptance handoff (test-owned; do not weaken in the build).
 * Test bank ref: test/queue-orientation-acceptance.
 * RED base: 9b034604bad8c489ee71effa7544132fe5017dfe (2026-10-23).
 * Observed: 17 assertion failures here, 18 existing queue regressions pass;
 * full web unit suite: 17 RED / 1655 pass. Web check: zero errors/warnings.
 *
 * Public contract: keep PlayerQueue's existing inputs and queue/transport hooks.
 * Add `queue-position` for visible, read-only "Queue selection" text containing
 * "Track X of N" and "Y upcoming". Empty: "Queue is empty". A nonempty queue
 * with an invalid index: "No track selected" and "N tracks", not a fake X.
 * Add a native button named "Show current track"; disabled without a current
 * row. No new store API or component input is required. The separate mocked
 * library browser spec exercises reactivity, explicit reveal and non-mutation.
 *
 * Reproduce from apps/web:
 *   pnpm install --frozen-lockfile
 *   pnpm test src/lib/player/playerQueue.orientation.test.ts
 * Regression: pnpm test src/lib/player/playerQueue.test.ts \
 *   src/lib/player/playerQueue.a11y.test.ts src/lib/player/playerQueueContracts.test.ts
 * Also run pnpm check and tests/library-queue-orientation.spec.ts (header).
 * Expected on the pre-feature base: assertion RED for missing queue-position /
 * Show current track, not an import/compile error. The Builder consumes these
 * tests in one combined ready PR; never merge the RED test bank by itself.
 *
 * Breaks caught: off-by-one/upcoming math; stale derived state after parent
 * snapshots; clamped/NaN indices posing as a selection; summary leaking into
 * the established now-playing announcement; added row tab stops or transport
 * state mutation. All expectations below are literal, hand-checked fixtures.
 */

function track(id: string, title: string): Track {
	return { id, title, artist: 'Orientation Band', streamUrl: `/test-audio/${id}`, artworkUrl: null };
}

const MANGO = track('orientation-mango', 'Mango Season');
const APPLE = track('orientation-apple', 'Apple Cart');
const ZEBRA = track('orientation-zebra', 'Zebra Crossing');
const KIWI = track('orientation-kiwi', 'Kiwi Stand');
const TRACKS = [MANGO, APPLE, ZEBRA];

function state(tracks: Track[], currentIndex: number): QueueState {
	return { ...createQueue(tracks), currentIndex };
}

function renderQueue(value: QueueState): string {
	return render(PlayerQueue, { props: { state: value } }).body;
}

/** Extract a hook's complete element, allowing nested elements of the same tag. */
function hook(body: string, testId: string): string | null {
	const opening = new RegExp(`<([a-zA-Z][\\w-]*)\\b[^>]*data-testid="${testId}"[^>]*>`).exec(body);
	if (!opening) return null;
	const tags = new RegExp(`</?${opening[1]}\\b[^>]*>`, 'g');
	tags.lastIndex = opening.index + opening[0].length;
	let depth = 1;
	for (let next = tags.exec(body); next; next = tags.exec(body)) {
		depth += next[0].startsWith('</') ? -1 : 1;
		if (depth === 0) return body.slice(opening.index, tags.lastIndex);
	}
	return null;
}

function text(markup: string | null): string {
	return (markup ?? '').replace(/<[^>]*>/g, ' ').replace(/\s+/g, ' ').trim();
}

function itemTags(body: string): string[] {
	return [...body.matchAll(/<[a-zA-Z][^>]*data-testid="queue-item"[^>]*>/g)].map((m) => m[0]);
}

function currentIds(body: string): (string | null)[] {
	return itemTags(body)
		.filter((tag) => /aria-current="true"/.test(tag))
		.map((tag) => tag.match(/data-track-id="([^"]+)"/)?.[1] ?? null);
}

function summary(body: string): string {
	const element = hook(body, 'queue-position');
	expect(element, 'visible read-only queue selection summary is missing').not.toBeNull();
	expect(text(element)).toMatch(/queue selection/i);
	return text(element);
}

const VALID_CASES = [
	{ id: 'QO-U01 singleton', value: state([MANGO], 0), position: 'Track 1 of 1', upcoming: '0 upcoming', selected: ['orientation-mango'] },
	{ id: 'QO-U02 first', value: state(TRACKS, 0), position: 'Track 1 of 3', upcoming: '2 upcoming', selected: ['orientation-mango'] },
	{ id: 'QO-U03 middle', value: state(TRACKS, 1), position: 'Track 2 of 3', upcoming: '1 upcoming', selected: ['orientation-apple'] },
	{ id: 'QO-U04 end', value: state(TRACKS, 2), position: 'Track 3 of 3', upcoming: '0 upcoming', selected: ['orientation-zebra'] }
];

describe('queue orientation — independent count and state contract', () => {
	it.each(VALID_CASES)('$id reports the selection, total and strictly subsequent tracks', ({ value, position, upcoming, selected }) => {
		const body = renderQueue(value);
		const label = summary(body);
		expect(label).toMatch(new RegExp(`\\b${position}\\b`));
		expect(label).toMatch(new RegExp(`\\b${upcoming}\\b`));
		expect(label).not.toMatch(/now playing|audible|is playing/i);
		expect(currentIds(body)).toEqual(selected);
	});

	it.each([
		{ id: 'QO-U05 canonical empty', value: createQueue([]) },
		{ id: 'QO-U06 empty with stale index', value: state([], 4) }
	])('$id has a clear empty selection and no current row', ({ value }) => {
		const body = renderQueue(value);
		expect(summary(body)).toMatch(/queue is empty/i);
		expect(summary(body)).not.toMatch(/Track \d+ of|-[0-9]|NaN|Infinity/);
		expect(itemTags(body)).toEqual([]);
		expect(currentIds(body)).toEqual([]);
	});

	it.each([
		{ id: 'QO-U07 negative', index: -1 },
		{ id: 'QO-U08 past end', index: 3 },
		{ id: 'QO-U09 fractional', index: 1.5 },
		{ id: 'QO-U10 NaN', index: Number.NaN },
		{ id: 'QO-U11 infinite', index: Number.POSITIVE_INFINITY }
	])('$id index is not clamped or advertised as a selected track', ({ index }) => {
		const body = renderQueue(state(TRACKS, index));
		const label = summary(body);
		expect(label).toMatch(/no track selected/i);
		expect(label).toMatch(/\b3 tracks\b/);
		expect(label).not.toMatch(/Track \S+ of|-[0-9]|NaN|Infinity|undefined/);
		expect(currentIds(body)).toEqual([]);
	});

	it('QO-U12 follows parent navigation/reorder/append/remove/clear snapshots without mutating them', () => {
		const snapshots = [
			{ value: state(TRACKS, 1), position: 'Track 2 of 3', upcoming: '1 upcoming', ids: ['orientation-mango', 'orientation-apple', 'orientation-zebra'], current: ['orientation-apple'] },
			{ value: state(TRACKS, 2), position: 'Track 3 of 3', upcoming: '0 upcoming', ids: ['orientation-mango', 'orientation-apple', 'orientation-zebra'], current: ['orientation-zebra'] },
			{ value: state([ZEBRA, MANGO, APPLE], 0), position: 'Track 1 of 3', upcoming: '2 upcoming', ids: ['orientation-zebra', 'orientation-mango', 'orientation-apple'], current: ['orientation-zebra'] },
			{ value: state([ZEBRA, MANGO, APPLE, KIWI], 0), position: 'Track 1 of 4', upcoming: '3 upcoming', ids: ['orientation-zebra', 'orientation-mango', 'orientation-apple', 'orientation-kiwi'], current: ['orientation-zebra'] },
			{ value: state([ZEBRA, APPLE, KIWI], 0), position: 'Track 1 of 3', upcoming: '2 upcoming', ids: ['orientation-zebra', 'orientation-apple', 'orientation-kiwi'], current: ['orientation-zebra'] },
			{ value: state([APPLE, KIWI], 1), position: 'Track 2 of 2', upcoming: '0 upcoming', ids: ['orientation-apple', 'orientation-kiwi'], current: ['orientation-kiwi'] }
		];
		for (const snapshot of snapshots) {
			const before = JSON.stringify(snapshot.value);
			Object.freeze(snapshot.value.tracks);
			Object.freeze(snapshot.value);
			const body = renderQueue(snapshot.value);
			expect(summary(body)).toMatch(new RegExp(`\\b${snapshot.position}\\b`));
			expect(summary(body)).toMatch(new RegExp(`\\b${snapshot.upcoming}\\b`));
			expect(itemTags(body).map((tag) => tag.match(/data-track-id="([^"]+)"/)?.[1])).toEqual(snapshot.ids);
			expect(currentIds(body)).toEqual(snapshot.current);
			expect(JSON.stringify(snapshot.value)).toBe(before);
		}
		expect(summary(renderQueue(createQueue([])))).toMatch(/queue is empty/i);
	});

	it('QO-U13 summary is outside the unchanged now-playing live region on append/reorder/remove', () => {
		const before = renderQueue(state(TRACKS, 1));
		const announcement = hook(before, 'now-playing');
		expect(announcement).not.toBeNull();
		expect(text(announcement)).toContain('Apple Cart');
		for (const value of [
			state([MANGO, APPLE, ZEBRA, KIWI], 1),
			state([ZEBRA, MANGO, APPLE], 2),
			state([APPLE, ZEBRA], 0)
		]) {
			const body = renderQueue(value);
			summary(body);
			expect(text(hook(body, 'now-playing'))).toBe(text(announcement));
			expect(announcement).not.toContain('queue-position');
			expect(text(announcement)).not.toMatch(/upcoming|Track \d+ of|queue selection/i);
		}
	});

	it('QO-U14 rows remain programmatic focus targets rather than extra normal tab stops', () => {
		const body = renderQueue(state(TRACKS, 1));
		const tags = itemTags(body);
		expect(tags).toHaveLength(3);
		for (const tag of tags) {
			const tabIndex = tag.match(/tabindex="([^"]+)"/)?.[1];
			expect(tabIndex === undefined || tabIndex === '-1', 'queue rows must not enter the normal tab order').toBe(true);
			if (/aria-current="true"/.test(tag)) {
				expect(tabIndex, 'current row needs a programmatic focus target').toBe('-1');
			}
		}
	});

	it.each([
		{ id: 'QO-U15 selected', value: state(TRACKS, 1), disabled: false },
		{ id: 'QO-U16 empty', value: createQueue([]), disabled: true },
		{ id: 'QO-U17 invalid', value: state(TRACKS, -1), disabled: true }
	])('$id exposes a native reveal button enabled only with a current row', ({ value, disabled }) => {
		const element = hook(renderQueue(value), 'queue-reveal-current');
		expect(element, 'queue-reveal-current native button is missing').not.toBeNull();
		expect(element).toMatch(/^<button\b/);
		const opening = element?.match(/^<button\b[^>]*>/)?.[0] ?? '';
		expect(opening).toMatch(/type="button"/);
		expect(/\sdisabled(?:[\s=>]|$)/.test(opening)).toBe(disabled);
		expect(text(element)).toMatch(/show current track/i);
	});
});
