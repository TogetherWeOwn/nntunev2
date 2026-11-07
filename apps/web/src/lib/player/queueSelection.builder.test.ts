/** Builder-owned tests for the pure queue multi-selection reducers; no UI is implied. */
import { describe, expect, it } from 'vitest';
import { createQueue, type QueueState, type Track } from './queue.js';
import {
	createQueueSelection, reconcileQueueSelection, selectAllQueueRows, toggleQueueSelection,
	type QueueSelection
} from './queueSelection.js';

const track = (id: string, title: string, artist = 'Artist', extra: Partial<Track> = {}): Track =>
	({ id, title, artist, streamUrl: `/${id}`, ...extra });
const TRACKS = [
	track('a', 'Alpha'), track('b', 'Beta'), track('c', 'Alpha Two'), track('d', 'Delta'), track('e', 'Alpha Three')
];
const queueOf = (...ids: string[]): QueueState =>
	createQueue(ids.map((id) => TRACKS.find((t) => t.id === id) ?? track(id, id.toUpperCase())));
const sel = (ids: string[], anchorId: string | null = null, active = true): QueueSelection => ({ ids, anchorId, active });
const freeze = <T extends QueueSelection>(s: T): T => { Object.freeze(s.ids); return Object.freeze(s); };

describe('queue selection reducers', () => {
	it('starts empty, inactive and anchorless, as a fresh object each time', () => {
		expect(createQueueSelection()).toEqual({ ids: [], anchorId: null, active: false });
		expect(createQueueSelection()).not.toBe(createQueueSelection());
	});

	it('toggles exact ids in activation order, anchors the toggled row and enters the mode', () => {
		const queue = queueOf('a', 'b', 'c', 'd');
		let s = toggleQueueSelection(createQueueSelection(), queue, '', 'c');
		s = toggleQueueSelection(s, queue, '', 'a');
		expect(s).toEqual({ ids: ['c', 'a'], anchorId: 'a', active: true });
		s = toggleQueueSelection(s, queue, '', 'c');
		expect(s).toEqual({ ids: ['a'], anchorId: 'c', active: true });
	});

	it('keeps the mode active when the last selected row is toggled off', () => {
		const queue = queueOf('a', 'b');
		const on = toggleQueueSelection(createQueueSelection(), queue, '', 'a');
		const off = toggleQueueSelection(on, queue, '', 'a');
		expect(off).toEqual({ ids: [], anchorId: 'a', active: true });
	});

	it('compares ids exactly: case, whitespace and lookalike ids are distinct rows', () => {
		const queue = createQueue([track('Abc', 'One'), track('abc', 'Two'), track('abc ', 'Three')]);
		let s = toggleQueueSelection(createQueueSelection(), queue, '', 'abc');
		expect(s.ids).toEqual(['abc']);
		s = toggleQueueSelection(s, queue, '', 'Abc');
		s = toggleQueueSelection(s, queue, '', 'abc ');
		expect(s.ids).toEqual(['abc', 'Abc', 'abc ']);
		const unchanged = toggleQueueSelection(s, queue, '', 'ABC');
		expect(unchanged).toBe(s);
	});

	it('ignores ids that are unknown, empty or hidden by the filter, returning the same object', () => {
		const queue = queueOf('a', 'b', 'c');
		const s = freeze(sel(['b'], 'b'));
		expect(toggleQueueSelection(s, queue, '', 'zzz')).toBe(s);
		expect(toggleQueueSelection(s, queue, '', '')).toBe(s);
		expect(toggleQueueSelection(s, queue, 'alpha', 'b')).toBe(s);
		expect(toggleQueueSelection(s, queue, 'alpha', 'b', true)).toBe(s);
		const initial = createQueueSelection();
		expect(toggleQueueSelection(initial, queue, '', 'zzz')).toBe(initial);
	});

	it('range adds the contiguous displayed run additively in both directions and keeps the anchor', () => {
		const queue = queueOf('a', 'b', 'c', 'd', 'e');
		const start = toggleQueueSelection(createQueueSelection(), queue, '', 'b');
		const forward = toggleQueueSelection(start, queue, '', 'd', true);
		expect(forward).toEqual({ ids: ['b', 'c', 'd'], anchorId: 'b', active: true });
		const backward = toggleQueueSelection(forward, queue, '', 'a', true);
		expect(backward).toEqual({ ids: ['b', 'c', 'd', 'a'], anchorId: 'b', active: true });
		const reverse = toggleQueueSelection(toggleQueueSelection(createQueueSelection(), queue, '', 'e'), queue, '', 'c', true);
		// Run members append in displayed-ascending order whatever the gesture direction.
		expect(reverse.ids).toEqual(['e', 'c', 'd']);
	});

	it('range never removes, preserves existing activation order and keeps selected rows in place', () => {
		const queue = queueOf('a', 'b', 'c', 'd', 'e');
		const s = freeze(sel(['d', 'b', 'e'], 'a'));
		const next = toggleQueueSelection(s, queue, '', 'e', true);
		expect(next.ids).toEqual(['d', 'b', 'e', 'a', 'c']);
		expect(s.ids).toEqual(['d', 'b', 'e']);
	});

	it('a range that adds nothing on an active selection is the same object', () => {
		const queue = queueOf('a', 'b', 'c');
		const s = freeze(sel(['a', 'b', 'c'], 'a'));
		expect(toggleQueueSelection(s, queue, '', 'c', true)).toBe(s);
	});

	it('range without a usable anchor degrades to a single toggle and anchors the focus', () => {
		const queue = queueOf('a', 'b', 'c');
		expect(toggleQueueSelection(createQueueSelection(), queue, '', 'c', true))
			.toEqual({ ids: ['c'], anchorId: 'c', active: true });
		const stale = toggleQueueSelection(sel(['a'], 'gone'), queue, '', 'c', true);
		expect(stale).toEqual({ ids: ['a', 'c'], anchorId: 'c', active: true });
	});

	it('range follows the current filtered arranged order and skips hidden rows between endpoints', () => {
		const queue = queueOf('a', 'b', 'c', 'd', 'e');
		const anchored = toggleQueueSelection(createQueueSelection(), queue, 'alpha', 'a');
		const ranged = toggleQueueSelection(anchored, queue, 'alpha', 'e', true);
		expect(ranged.ids).toEqual(['a', 'c', 'e']);
		expect(ranged.ids).not.toContain('b');
		expect(ranged.ids).not.toContain('d');
	});

	it('range uses the arrangement at call time, not the order the anchor was set in', () => {
		const original = queueOf('a', 'b', 'c', 'd');
		const anchored = toggleQueueSelection(createQueueSelection(), original, '', 'a');
		const rearranged = queueOf('d', 'a', 'c', 'b');
		const ranged = toggleQueueSelection(anchored, rearranged, '', 'b', true);
		expect(ranged.ids).toEqual(['a', 'c', 'b']);
	});

	it('a range does not widen to rows hidden since the anchor was set (hidden anchor degrades)', () => {
		const queue = queueOf('a', 'b', 'c', 'd', 'e');
		const anchored = sel(['b'], 'b');
		const next = toggleQueueSelection(anchored, queue, 'alpha', 'e', true);
		expect(next).toEqual({ ids: ['b', 'e'], anchorId: 'e', active: true });
	});

	it('select all adds displayed rows in queue order, retaining hidden membership first', () => {
		const queue = queueOf('a', 'b', 'c', 'd', 'e');
		const s = freeze(sel(['d', 'b'], 'b'));
		const all = selectAllQueueRows(s, queue, 'alpha');
		expect(all).toEqual({ ids: ['d', 'b', 'a', 'c', 'e'], anchorId: 'b', active: true });
		expect(s.ids).toEqual(['d', 'b']);
	});

	it('select all without a filter selects the whole queue and enters the mode', () => {
		const queue = queueOf('c', 'a', 'b');
		expect(selectAllQueueRows(createQueueSelection(), queue, '')).toEqual({ ids: ['c', 'a', 'b'], anchorId: null, active: true });
	});

	it('select all with nothing displayed is a no-op returning the same object', () => {
		const queue = queueOf('a', 'b');
		const s = createQueueSelection();
		expect(selectAllQueueRows(s, queue, 'no such row')).toBe(s);
		const empty = createQueueSelection();
		expect(selectAllQueueRows(empty, createQueue([]), '')).toBe(empty);
	});

	it('select all is idempotent by identity once complete and active', () => {
		const queue = queueOf('a', 'b');
		const first = selectAllQueueRows(createQueueSelection(), queue, '');
		expect(selectAllQueueRows(first, queue, '')).toBe(first);
	});

	it('select all activates an inactive selection even when every row is already selected', () => {
		const queue = queueOf('a', 'b');
		const inactive = sel(['a', 'b'], null, false);
		const result = selectAllQueueRows(inactive, queue, '');
		expect(result).not.toBe(inactive);
		expect(result).toEqual({ ids: ['a', 'b'], anchorId: null, active: true });
	});

	it('reconcile prunes only ids absent from the whole queue, never ids merely filtered out', () => {
		const queue = queueOf('a', 'b', 'c');
		const s = freeze(sel(['c', 'gone', 'b'], null));
		const next = reconcileQueueSelection(s, queue, 'alpha');
		expect(next.ids).toEqual(['c', 'b']);
		expect(next.active).toBe(true);
	});

	it('reconcile clears an anchor that is hidden by the filter or removed from the queue', () => {
		const queue = queueOf('a', 'b', 'c');
		expect(reconcileQueueSelection(sel(['b'], 'b'), queue, 'alpha')).toEqual({ ids: ['b'], anchorId: null, active: true });
		expect(reconcileQueueSelection(sel(['b'], 'gone'), queue, '')).toEqual({ ids: ['b'], anchorId: null, active: true });
		expect(reconcileQueueSelection(sel(['a'], 'a'), queue, 'alpha').anchorId).toBe('a');
	});

	it('reconcile does not restore a pruned anchor or a pruned id when it returns', () => {
		const hidden = reconcileQueueSelection(sel(['b'], 'b'), queueOf('a', 'b'), 'alpha');
		const shown = reconcileQueueSelection(hidden, queueOf('a', 'b'), '');
		expect(shown.anchorId).toBeNull();
		const removed = reconcileQueueSelection(sel(['b']), queueOf('a'), '');
		expect(reconcileQueueSelection(removed, queueOf('a', 'b'), '').ids).toEqual([]);
	});

	it('reconcile keeps activation order, not queue order', () => {
		const queue = queueOf('a', 'b', 'c');
		const next = reconcileQueueSelection(sel(['c', 'x', 'a']), queue, '');
		expect(next.ids).toEqual(['c', 'a']);
	});

	it('reconcile ends the mode when the queue is empty and keeps it when only the selection empties', () => {
		const ended = reconcileQueueSelection(sel(['a'], 'a'), createQueue([]), '');
		expect(ended).toEqual({ ids: [], anchorId: null, active: false });
		const kept = reconcileQueueSelection(sel(['a'], null), queueOf('b'), '');
		expect(kept).toEqual({ ids: [], anchorId: null, active: true });
	});

	it('reconcile returns the same object when nothing changes, including inactive empty state', () => {
		const queue = queueOf('a', 'b');
		const s = freeze(sel(['b', 'a'], 'a'));
		expect(reconcileQueueSelection(s, queue, '')).toBe(s);
		const initial = createQueueSelection();
		expect(reconcileQueueSelection(initial, queue, 'alpha')).toBe(initial);
		expect(reconcileQueueSelection(initial, createQueue([]), '')).toBe(initial);
	});

	it('does not read or alter current-track or queue references', () => {
		const queue = Object.freeze({ ...queueOf('a', 'b', 'c'), tracks: Object.freeze([...queueOf('a', 'b', 'c').tracks]) as Track[] });
		const before = JSON.stringify(queue);
		toggleQueueSelection(createQueueSelection(), queue, 'a', 'a');
		selectAllQueueRows(createQueueSelection(), queue, '');
		reconcileQueueSelection(sel(['a']), queue, 'b');
		expect(JSON.stringify(queue)).toBe(before);
	});
});

describe('queue selection matcher parity', () => {
	it('uses the stable id for a synthesized blank title but not for a real title equal to another id', () => {
		const queue = createQueue([
			track('needle-1', '', 'Artist', { titleIsFallback: true }),
			track('plain', 'Plain title'),
			track('hay', 'needle-1 is only a title')
		]);
		expect(selectAllQueueRows(createQueueSelection(), queue, 'NEEDLE-1').ids).toEqual(['needle-1', 'hay']);
		expect(selectAllQueueRows(createQueueSelection(), queue, 'plain').ids).toEqual(['plain']);
	});

	it('does not match a real title against the id when the title is present', () => {
		const queue = createQueue([track('hidden-id', 'Visible title')]);
		const s = createQueueSelection();
		expect(toggleQueueSelection(s, queue, 'hidden-id', 'hidden-id')).toBe(s);
	});

	it('matches canonically equivalent accented metadata, case-folded, in either direction', () => {
		const composed = 'Caf\u00e9';
		const decomposed = 'Cafe\u0301';
		const queue = createQueue([track('x', composed), track('y', decomposed, 'Z'), track('z', 'Cafe')]);
		expect(selectAllQueueRows(createQueueSelection(), queue, decomposed).ids).toEqual(['x', 'y']);
		expect(selectAllQueueRows(createQueueSelection(), queue, composed.toUpperCase()).ids).toEqual(['x', 'y']);
	});

	it('matches artist text and treats a missing artist as empty, never "undefined"', () => {
		const queue = createQueue([
			track('m', 'One', 'Miles'), { id: 'n', title: 'Two', streamUrl: '/n' } as unknown as Track
		]);
		expect(selectAllQueueRows(createQueueSelection(), queue, 'miles').ids).toEqual(['m']);
		expect(selectAllQueueRows(createQueueSelection(), queue, 'undefined')).toEqual(createQueueSelection());
	});

	it('treats a whitespace-only query as unfiltered', () => {
		const queue = queueOf('a', 'b');
		expect(selectAllQueueRows(createQueueSelection(), queue, '   ').ids).toEqual(['a', 'b']);
	});
});
