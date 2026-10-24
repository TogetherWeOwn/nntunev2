import { describe, expect, it } from 'vitest';
import { render } from 'svelte/server';
import PlayerQueue from './PlayerQueue.svelte';
import { createQueue, currentTrack, nextTrack, selectTrack, type Track } from './queue.js';

/**
 * PICK — independent test bank, paired with tests/library-queue-direct-selection.spec.ts.
 * Public callback contract: optional onselect(index: number), following the existing
 * onremove/onplaynext coordinate convention. It selects metadata, never playback.
 * No callback means no enabled inert Select action; omitted or disabled is valid.
 * Builder: preserve these new files and existing queue/management/orientation tests.
 * Run: pnpm --dir apps/web test src/lib/player/playerQueue.direct-selection.acceptance.test.ts
 * Breaks caught: reordered selection, clamped invalid coordinates, wrong current
 * marker, implicit playback language and unavailable host actions advertised as live.
 * Existing validated transitions are positive controls, NOT claimed as feature RED.
 */
const TRACKS: Track[] = [
	{ id: 'pick-zulu', title: 'Zulu Song', artist: 'Pick Band', streamUrl: '', artworkUrl: null },
	{ id: 'pick-alpha', title: 'Alpha Song', artist: 'Pick Band', streamUrl: '', artworkUrl: null },
	{ id: 'pick-mike', title: 'Mike Song', artist: 'Pick Band', streamUrl: '', artworkUrl: null }
];

function currentIds(body: string) {
	return [...body.matchAll(/<[^>]+data-testid="queue-item"[^>]*>/g)]
		.filter(([tag]) => /aria-current="true"/.test(tag))
		.map(([tag]) => tag.match(/data-track-id="([^"]+)"/)?.[1]);
}

function selectButtons(body: string) {
	return [...body.matchAll(/<button\b([^>]*)>([\s\S]*?)<\/button>/g)]
		.filter(([, attrs, content]) => /aria-label="Select [^"]+"/.test(attrs!) || /^Select\s/.test(content!.replace(/<[^>]*>/g, '').trim()));
}

function frozenQueue() {
	const value = { ...createQueue(TRACKS), currentIndex: 1 };
	value.tracks.forEach(Object.freeze);
	Object.freeze(value.tracks);
	return Object.freeze(value);
}

describe('PICK state transitions and shared component contract', () => {
	it.each([
		{ id: 'PICK-U01', index: 0, selected: 'pick-zulu', next: 'pick-alpha' },
		{ id: 'PICK-U02', index: 1, selected: 'pick-alpha', next: 'pick-mike' },
		{ id: 'PICK-U03', index: 2, selected: 'pick-mike', next: 'pick-mike' }
	])('$id selects the original coordinate without arranging or mutating tracks', ({ index, selected, next }) => {
		const before = frozenQueue();
		const result = selectTrack(before, index);
		expect(result.currentIndex).toBe(index);
		expect(currentTrack(result)?.id).toBe(selected);
		expect(result.tracks.map((track) => track.id)).toEqual(['pick-zulu', 'pick-alpha', 'pick-mike']);
		expect(result.tracks).toBe(before.tracks);
		expect(before.currentIndex).toBe(1);
		expect(currentIds(render(PlayerQueue, { props: { state: result } }).body)).toEqual([selected]);
		expect(currentTrack(nextTrack(result))?.id).toBe(next);
	});

	it.each([
		{ id: 'PICK-U04', index: -1 },
		{ id: 'PICK-U05', index: 3 },
		{ id: 'PICK-U06', index: 0.5 },
		{ id: 'PICK-U07', index: Number.NaN },
		{ id: 'PICK-U08', index: Number.POSITIVE_INFINITY },
		{ id: 'PICK-U09', index: Number.NEGATIVE_INFINITY },
		{ id: 'PICK-U10', index: Number.MAX_SAFE_INTEGER }
	])('$id rejects an invalid coordinate instead of selecting another track', ({ index }) => {
		const before = frozenQueue();
		expect(selectTrack(before, index)).toBe(before);
		expect(currentTrack(before)?.id).toBe('pick-alpha');
	});

	it('PICK-U11 selecting the current coordinate is an observable state no-op', () => {
		const before = frozenQueue();
		expect(selectTrack(before, 1)).toEqual(before);
	});

	it('PICK-U12 an empty queue cannot select a phantom row', () => {
		const before = createQueue([]);
		expect(selectTrack(before, 0)).toBe(before);
		expect(currentTrack(before)).toBeNull();
	});

	it('PICK-U13 an available host callback exposes a native metadata Select button per row', () => {
		// A spread keeps the pre-feature component compilable: missing support must
		// fail at the rendered-control assertion, never at TypeScript setup.
		const props = { state: frozenQueue(), onselect: (_index: number) => {} };
		const buttons = selectButtons(render(PlayerQueue, { props }).body);
		expect(buttons, 'missing host-owned direct-selection controls').toHaveLength(3);
		for (const title of ['Zulu Song', 'Alpha Song', 'Mike Song']) {
			const button = buttons.find(([, attrs, content]) => attrs?.includes(`aria-label="Select ${title}"`) || content?.replace(/<[^>]*>/g, '').trim() === `Select ${title}`);
			expect(button, `missing Select ${title}`).toBeDefined();
			expect(button?.[1]).toMatch(/type="button"/);
			expect(button?.[0]).not.toMatch(/play now|stream available|is playing/i);
		}
	});

	it('PICK-U14 without a host callback no enabled inert Select action is rendered', () => {
		const buttons = selectButtons(render(PlayerQueue, { props: { state: frozenQueue() } }).body);
		for (const [, attrs] of buttons) expect(attrs).toMatch(/\sdisabled(?:\s|=|$)/);
	});
});
