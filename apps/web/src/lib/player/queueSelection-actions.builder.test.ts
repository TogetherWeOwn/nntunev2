/** Builder-owned tests for applyQueueSelection: atomic, immutable, stable-id queue edits. */
import { describe, expect, it } from 'vitest';
import { playNext, removeTrack, type QueueState, type Track } from './queue.js';
import { matchesQueueQuery } from './queueFilter.js';
import { applyQueueSelection, createQueueSelection, toggleQueueSelection } from './queueSelection.js';

const track = (id: string, title = id.toUpperCase()): Track => ({ id, title, artist: 'Artist', streamUrl: `/${id}` });
const LETTERS = ['a', 'b', 'c', 'd', 'e', 'f'];
const POOL = LETTERS.map((id) => track(id));
const freezeQueue = (tracks: Track[], currentIndex: number): QueueState => {
	tracks.forEach((t) => Object.freeze(t));
	return Object.freeze({ tracks: Object.freeze(tracks) as Track[], currentIndex });
};
const mk = (ids: string[], currentIndex: number): QueueState =>
	freezeQueue(ids.map((id) => POOL.find((t) => t.id === id) ?? track(id)), currentIndex);
const idsOf = (q: QueueState) => q.tracks.map((t) => t.id);
const frozenIds = (ids: string[]) => Object.freeze([...ids]);

describe('applyQueueSelection remove', () => {
	it('removes selected rows in arranged order, not selection order, keeping survivors by reference', () => {
		const q = mk(['a', 'b', 'c', 'd', 'e'], 2);
		const out = applyQueueSelection(q, frozenIds(['d', 'a']), 'remove');
		expect(idsOf(out)).toEqual(['b', 'c', 'e']);
		expect(out.currentIndex).toBe(1);
		expect(out.tracks[1]).toBe(q.tracks[2]);
		expect(out.tracks[0]).toBe(q.tracks[1]);
		expect(out.tracks[2]).toBe(q.tracks[4]);
		expect(out).not.toBe(q);
		expect(out.tracks).not.toBe(q.tracks);
		expect(idsOf(q)).toEqual(['a', 'b', 'c', 'd', 'e']);
		expect(q.currentIndex).toBe(2);
	});

	it('ignores stale and duplicate ids and acts once per row', () => {
		const q = mk(['a', 'b', 'c'], 0);
		const out = applyQueueSelection(q, frozenIds(['c', 'zzz', 'c', 'c', '', 'nope']), 'remove');
		expect(idsOf(out)).toEqual(['a', 'b']);
		expect(out.currentIndex).toBe(0);
	});

	it('returns the same state when nothing selected exists', () => {
		const q = mk(['a', 'b'], 1);
		expect(applyQueueSelection(q, frozenIds([]), 'remove')).toBe(q);
		expect(applyQueueSelection(q, frozenIds(['x', 'y', 'x']), 'remove')).toBe(q);
	});

	it('moves current to the next surviving row when current is removed', () => {
		const q = mk(['a', 'b', 'c', 'd', 'e'], 1);
		const out = applyQueueSelection(q, frozenIds(['b', 'c']), 'remove');
		expect(idsOf(out)).toEqual(['a', 'd', 'e']);
		expect(out.currentIndex).toBe(1);
		expect(out.tracks[out.currentIndex]).toBe(q.tracks[3]);
	});

	it('falls back to the previous surviving row when no later row survives', () => {
		const q = mk(['a', 'b', 'c', 'd'], 2);
		const out = applyQueueSelection(q, frozenIds(['c', 'd']), 'remove');
		expect(idsOf(out)).toEqual(['a', 'b']);
		expect(out.currentIndex).toBe(1);
		expect(out.tracks[1]).toBe(q.tracks[1]);
	});

	it('empties to -1 when every row is removed, including with hidden extras', () => {
		const q = mk(['a', 'b'], 1);
		const out = applyQueueSelection(q, frozenIds(['b', 'q', 'a']), 'remove');
		expect(out.tracks).toEqual([]);
		expect(out.currentIndex).toBe(-1);
	});

	it('keeps a no-current queue at -1', () => {
		const q = mk(['a', 'b', 'c'], -1);
		const out = applyQueueSelection(q, frozenIds(['b']), 'remove');
		expect(idsOf(out)).toEqual(['a', 'c']);
		expect(out.currentIndex).toBe(-1);
	});
});

describe('applyQueueSelection playNext', () => {
	it('places selected rows after current in arranged order regardless of selection order', () => {
		const q = mk(['a', 'b', 'c', 'd', 'e'], 1);
		const out = applyQueueSelection(q, frozenIds(['e', 'a', 'd']), 'playNext');
		expect(idsOf(out)).toEqual(['b', 'a', 'd', 'e', 'c']);
		expect(out.currentIndex).toBe(0);
		expect(out.tracks[0]).toBe(q.tracks[1]);
		expect(out.tracks[1]).toBe(q.tracks[0]);
		expect(out.tracks[4]).toBe(q.tracks[2]);
		expect(idsOf(q)).toEqual(['a', 'b', 'c', 'd', 'e']);
	});

	it('handles stale and duplicate ids and never duplicates rows', () => {
		const q = mk(['a', 'b', 'c', 'd'], 0);
		const out = applyQueueSelection(q, frozenIds(['d', 'ghost', 'd', 'c', 'c']), 'playNext');
		expect(idsOf(out)).toEqual(['a', 'c', 'd', 'b']);
		expect(out.currentIndex).toBe(0);
	});

	it('is a no-op without a current track', () => {
		const q = mk(['a', 'b', 'c'], -1);
		expect(applyQueueSelection(q, frozenIds(['a', 'c']), 'playNext')).toBe(q);
	});

	it('is a no-op when only the current row is selected, even with stale ids', () => {
		const q = mk(['a', 'b', 'c'], 1);
		expect(applyQueueSelection(q, frozenIds(['b']), 'playNext')).toBe(q);
		expect(applyQueueSelection(q, frozenIds(['b', 'b', 'gone']), 'playNext')).toBe(q);
	});

	it('returns the same state when the arrangement is unchanged', () => {
		const q = mk(['a', 'b', 'c', 'd'], 1);
		expect(applyQueueSelection(q, frozenIds(['c']), 'playNext')).toBe(q);
		expect(applyQueueSelection(q, frozenIds(['d', 'c', 'b']), 'playNext')).toBe(q);
		expect(applyQueueSelection(q, frozenIds([]), 'playNext')).toBe(q);
		expect(applyQueueSelection(q, frozenIds(['nope']), 'playNext')).toBe(q);
	});

	it('moves selected rows from before current and keeps current identity', () => {
		const q = mk(['a', 'b', 'c', 'd'], 3);
		const out = applyQueueSelection(q, frozenIds(['b', 'a']), 'playNext');
		expect(idsOf(out)).toEqual(['c', 'd', 'a', 'b']);
		expect(out.currentIndex).toBe(1);
		expect(out.tracks[1]).toBe(q.tracks[3]);
	});

	it('includes rows hidden by a filter when ids come from a selection', () => {
		const titled = [track('a', 'Alpha'), track('b', 'Beta'), track('c', 'Alpha Two'), track('d', 'Delta')];
		const q = freezeQueue(titled, 3);
		let selection = createQueueSelection();
		selection = toggleQueueSelection(selection, q, 'alpha', 'c');
		selection = toggleQueueSelection(selection, q, 'alpha', 'a');
		expect(matchesQueueQuery(titled[2]!, 'delta')).toBe(false);
		expect(matchesQueueQuery(titled[0]!, 'delta')).toBe(false);
		const out = applyQueueSelection(q, selection.ids, 'playNext');
		expect(idsOf(out)).toEqual(['b', 'd', 'a', 'c']);
		expect(out.currentIndex).toBe(1);
		const removed = applyQueueSelection(q, selection.ids, 'remove');
		expect(idsOf(removed)).toEqual(['b', 'd']);
		expect(removed.currentIndex).toBe(1);
	});
});

/** Every subset of ids as an input list, in several orders with duplicates and stale ids mixed in. */
function inputVariants(subset: string[]): string[][] {
	return [subset, [...subset].reverse(), [...subset, ...subset, 'stale-1'], ['stale-2', ...[...subset].reverse(), 'stale-1']];
}

/** Independent oracle: existing removeTrack applied at descending original indices. */
function removeOracle(q: QueueState, selectedIdx: number[]): QueueState {
	let state: QueueState = q;
	for (const index of [...selectedIdx].sort((x, y) => y - x)) state = removeTrack(state, index);
	return state;
}

/** Independent oracle: existing playNext over selected original arrangement in reverse. */
function playNextOracle(q: QueueState, selectedIdx: number[]): QueueState {
	const currentTrack = q.tracks[q.currentIndex];
	let state: QueueState = q;
	for (const index of [...selectedIdx].sort((x, y) => y - x)) {
		const t = q.tracks[index]!;
		if (t === currentTrack) continue;
		state = playNext(state, state.tracks.indexOf(t));
	}
	return state;
}

describe('applyQueueSelection exhaustive small-queue oracles', () => {
	const cases: Array<{ n: number; current: number; mask: number }> = [];
	for (let n = 0; n <= 5; n += 1) {
		for (let current = -1; current < n; current += 1) {
			for (let mask = 0; mask < 1 << n; mask += 1) cases.push({ n, current, mask });
		}
	}

	it('covers a meaningful number of cases', () => {
		expect(cases.length).toBe(
			[0, 1, 2, 3, 4, 5].reduce((sum, n) => sum + (n + 1) * 2 ** n, 0)
		);
	});

	it('remove matches descending removeTrack for every subset and current index', () => {
		for (const { n, current, mask } of cases) {
			if (n === 0 && current !== -1) continue;
			const q = mk(LETTERS.slice(0, n), n === 0 ? -1 : current);
			const idx = [...Array(n).keys()].filter((i) => mask & (1 << i));
			const expected = removeOracle(q, idx);
			for (const input of inputVariants(idx.map((i) => LETTERS[i]!))) {
				const label = `n=${n} current=${current} mask=${mask} ids=${input.join(',')}`;
				const out = applyQueueSelection(q, frozenIds(input), 'remove');
				if (idx.length === 0) {
					expect(out, label).toBe(q);
					continue;
				}
				expect(idsOf(out), label).toEqual(idsOf(expected));
				expect(out.currentIndex, label).toBe(expected.currentIndex);
				out.tracks.forEach((t, i) => expect(t, label).toBe(expected.tracks[i]));
				expect(Object.is(out, q), label).toBe(false);
				// Surviving current identity preserved whenever current was not selected.
				if (current >= 0 && !(mask & (1 << current))) {
					expect(out.tracks[out.currentIndex], label).toBe(q.tracks[current]);
				}
				expect(idsOf(q), label).toEqual(LETTERS.slice(0, n));
				expect(q.currentIndex, label).toBe(n === 0 ? -1 : current);
			}
		}
	});

	it('playNext matches reverse-iterated playNext, with same-arrangement as a same-state no-op', () => {
		let changed = 0;
		let unchanged = 0;
		for (const { n, current, mask } of cases) {
			if (n === 0 && current !== -1) continue;
			const q = mk(LETTERS.slice(0, n), n === 0 ? -1 : current);
			const idx = [...Array(n).keys()].filter((i) => mask & (1 << i));
			const expected = playNextOracle(q, idx);
			const sameArrangement = expected.tracks.every((t, i) => t === q.tracks[i]);
			for (const input of inputVariants(idx.map((i) => LETTERS[i]!))) {
				const label = `n=${n} current=${current} mask=${mask} ids=${input.join(',')}`;
				const out = applyQueueSelection(q, frozenIds(input), 'playNext');
				if (current < 0 || sameArrangement) {
					expect(out, label).toBe(q);
					unchanged += 1;
					continue;
				}
				changed += 1;
				expect(idsOf(out), label).toEqual(idsOf(expected));
				expect(out.currentIndex, label).toBe(expected.currentIndex);
				out.tracks.forEach((t, i) => expect(t, label).toBe(expected.tracks[i]));
				expect(out.tracks[out.currentIndex], label).toBe(q.tracks[current]);
				expect(new Set(idsOf(out)).size, label).toBe(n);
				expect(Object.is(out, q), label).toBe(false);
				expect(idsOf(q), label).toEqual(LETTERS.slice(0, n));
				expect(q.currentIndex, label).toBe(current);
			}
		}
		expect(changed).toBeGreaterThan(100);
		expect(unchanged).toBeGreaterThan(100);
	});
});
