import { describe, expect, it } from 'vitest';
import { applyQueueAction, type QueueAction } from './queueActions.js';
import type { QueueState, Track } from './queue.js';

const t = (id: string, title = `Track ${id.toUpperCase()}`): Track => ({ id, title, artist: 'Artist', streamUrl: `/${id}` });
const state = (ids: string[], currentIndex: number, titles: Record<string, string> = {}): QueueState =>
	({ tracks: ids.map((id) => t(id, titles[id])), currentIndex });

describe('applyQueueAction commands and exact feedback', () => {
	it('move keeps the current track and reports a 1-based destination without repeat', () => {
		const before = state(['a', 'b', 'c'], 1);
		const result = applyQueueAction(before, { type: 'move', from: 0, to: 2 })!;
		expect(result.queue.tracks.map((x) => x.id)).toEqual(['b', 'c', 'a']);
		expect(result.queue.tracks[result.queue.currentIndex]!.id).toBe('b');
		expect(result.feedback).toBe('Track A moved to 3 of 3');
		expect(result.repeat).toBe(false);
	});

	it('remove reports the title, asks for a repeat announcement and shifts current', () => {
		const result = applyQueueAction(state(['a', 'b', 'c'], 2), { type: 'remove', index: 0 })!;
		expect(result.queue.tracks.map((x) => x.id)).toEqual(['b', 'c']);
		expect(result.queue.currentIndex).toBe(1);
		expect(result.feedback).toBe('Track A removed from the queue.');
		expect(result.repeat).toBe(true);
	});

	it('select moves current without repeat', () => {
		const result = applyQueueAction(state(['a', 'b', 'c'], 0), { type: 'select', index: 2 })!;
		expect(result.queue.currentIndex).toBe(2);
		expect(result.feedback).toBe('Selected Track C');
		expect(result.repeat).toBe(false);
	});

	it('clear empties the queue and repeats', () => {
		const result = applyQueueAction(state(['a', 'b'], 1), { type: 'clear' })!;
		expect(result.queue).toEqual({ tracks: [], currentIndex: -1 });
		expect(result.feedback).toBe('Queue cleared.');
		expect(result.repeat).toBe(true);
	});

	it('trimPrevious pluralises and keeps current at index 0', () => {
		const one = applyQueueAction(state(['a', 'b', 'c'], 1), { type: 'trimPrevious' })!;
		expect(one.queue.tracks.map((x) => x.id)).toEqual(['b', 'c']);
		expect(one.queue.currentIndex).toBe(0);
		expect(one.feedback).toBe('Removed 1 track before the current track.');
		expect(one.repeat).toBe(false);
		const two = applyQueueAction(state(['a', 'b', 'c'], 2), { type: 'trimPrevious' })!;
		expect(two.feedback).toBe('Removed 2 tracks before the current track.');
	});

	it('trimUpcoming pluralises and keeps current slot', () => {
		const one = applyQueueAction(state(['a', 'b', 'c'], 1), { type: 'trimUpcoming' })!;
		expect(one.queue.tracks.map((x) => x.id)).toEqual(['a', 'b']);
		expect(one.queue.currentIndex).toBe(1);
		expect(one.feedback).toBe('Removed 1 upcoming track.');
		const two = applyQueueAction(state(['a', 'b', 'c'], 0), { type: 'trimUpcoming' })!;
		expect(two.feedback).toBe('Removed 2 upcoming tracks.');
		expect(two.repeat).toBe(false);
	});
});

describe('applyQueueAction no-ops', () => {
	const base = state(['a', 'b', 'c'], 1);
	const snapshot = JSON.stringify(base);
	const invalid: QueueAction[] = [
		{ type: 'move', from: NaN, to: 0 }, { type: 'move', from: 0, to: NaN },
		{ type: 'move', from: 0.5, to: 2 }, { type: 'move', from: 0, to: 1.5 },
		{ type: 'move', from: -1, to: 1 }, { type: 'move', from: 0, to: 3 },
		{ type: 'move', from: 3, to: 0 }, { type: 'move', from: 1, to: 1 },
		{ type: 'remove', index: NaN }, { type: 'remove', index: 1.5 },
		{ type: 'remove', index: -1 }, { type: 'remove', index: 3 },
		{ type: 'select', index: NaN }, { type: 'select', index: 0.5 },
		{ type: 'select', index: -1 }, { type: 'select', index: 3 },
		{ type: 'select', index: 1 }
	];
	it.each(invalid.map((a) => [JSON.stringify(a) + ' ' + a.type, a] as const))('%s returns null and leaves the input untouched', (_n, action) => {
		expect(applyQueueAction(base, action)).toBeNull();
		expect(JSON.stringify(base)).toBe(snapshot);
	});

	it('trims and clear are silent when nothing would change', () => {
		expect(applyQueueAction(state(['a', 'b'], 0), { type: 'trimPrevious' })).toBeNull();
		expect(applyQueueAction(state(['a', 'b'], 1), { type: 'trimUpcoming' })).toBeNull();
		const empty = state([], -1);
		for (const type of ['clear', 'trimPrevious', 'trimUpcoming'] as const) {
			expect(applyQueueAction(empty, { type })).toBeNull();
		}
		expect(applyQueueAction(empty, { type: 'select', index: 0 })).toBeNull();
		expect(applyQueueAction(state(['a', 'b'], -1), { type: 'trimPrevious' })).toBeNull();
		expect(applyQueueAction(state(['a', 'b'], 5), { type: 'trimUpcoming' })).toBeNull();
	});

	it('never mutates track arrays on success either', () => {
		const before = state(['a', 'b', 'c'], 1);
		const copy = JSON.stringify(before);
		const tracks = before.tracks;
		for (const action of [
			{ type: 'move', from: 0, to: 2 }, { type: 'remove', index: 1 }, { type: 'select', index: 2 },
			{ type: 'clear' }, { type: 'trimPrevious' }, { type: 'trimUpcoming' }
		] as QueueAction[]) applyQueueAction(before, action);
		expect(JSON.stringify(before)).toBe(copy);
		expect(before.tracks).toBe(tracks);
	});
});

describe('applyQueueAction title fallbacks', () => {
	it.each(['', '   ', '\t\n'])('remove and select fall back to the id for title %j', (title) => {
		const q = state(['a', 'b', 'c'], 0, { b: title });
		expect(applyQueueAction(q, { type: 'remove', index: 1 })!.feedback).toBe('b removed from the queue.');
		expect(applyQueueAction(q, { type: 'select', index: 1 })!.feedback).toBe('Selected b');
	});

	it('keeps a padded real title verbatim', () => {
		const q = state(['a', 'b'], 0, { b: ' Padded ' });
		expect(applyQueueAction(q, { type: 'select', index: 1 })!.feedback).toBe('Selected  Padded ');
	});

	it('removing two same-titled tracks yields identical text with repeat set so the version can advance', () => {
		const q = state(['a', 'b', 'c'], 0, { b: 'Same', c: 'Same' });
		const first = applyQueueAction(q, { type: 'remove', index: 1 })!;
		const second = applyQueueAction(first.queue, { type: 'remove', index: 1 })!;
		expect(first.feedback).toBe('Same removed from the queue.');
		expect(second.feedback).toBe(first.feedback);
		expect(first.repeat && second.repeat).toBe(true);
	});
});
