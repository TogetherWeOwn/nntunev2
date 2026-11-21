import { expect, test, vi } from 'vitest';
import { createCurrentTrackFavorite } from './currentTrackFavorite.svelte.js';
import { createQueue, DEMO_TRACKS, selectTrack } from './queue.js';
import { createQueueController } from './queueController.js';

function setup() {
	const queue = createQueueController(createQueue(DEMO_TRACKS));
	const favorite = createCurrentTrackFavorite(queue);
	const edits = new Map<string, { pressed: boolean; edit: number }>();
	const read = vi.fn((id: string): { pressed: boolean; edit: number } | null => edits.get(id) ?? { pressed: false, edit: 0 });
	const write = vi.fn(async (id: string, pressed: boolean) => {
		edits.set(id, { pressed, edit: (edits.get(id)?.edit ?? 0) + 1 });
		return 'saved' as const;
	});
	const dispose = favorite.bind({ read, write });
	const select = (index: number) => {
		const next = selectTrack(queue.snapshot, index);
		queue.publish(next, queue.begin(next, 'select'));
	};
	return { favorite, view: () => favorite.view(queue.snapshot), read, write, select, dispose };
}

test('writes just the selected track, once per rendered intent, and reports only after settlement', async () => {
	const { favorite, select, view, write } = setup();
	select(1);
	const before = view()!;
	const pending = favorite.activate(before);
	expect(favorite.feedback(view())).toBeNull();
	await Promise.all([favorite.activate(before), pending]);
	expect(favorite.feedback(view())?.text).toBe('Starred Second Signal.');
	await favorite.activate(view()!);
	expect(write.mock.calls).toEqual([['second-signal', true], ['second-signal', false]]);
	expect(favorite.feedback(view())?.text).toBe('Unstarred Second Signal.');
});

test.each(['selection round trip', 'owner refusal'] as const)('%s cannot write or announce', (change) => {
	const { favorite, select, view, read, write } = setup();
	const before = view()!;
	if (change === 'selection round trip') { select(1); select(0); }
	else read.mockReturnValue(null);
	void favorite.activate(before);
	expect([write.mock.calls, favorite.feedback(view())]).toEqual([[], null]);
});

test('disposing the live owner prevents replay and late feedback across replacement', async () => {
	const { favorite, view, write, dispose } = setup();
	const before = view()!;
	const pending = favorite.activate(before);
	dispose();
	void favorite.activate(before);
	expect([view(), favorite.feedback(view())]).toEqual([null, null]);
	expect(write).toHaveBeenCalledTimes(1);
	const replacement = vi.fn(async () => null);
	favorite.bind({ read: () => before, write: replacement });
	await Promise.all([favorite.activate(before), pending]);
	expect([replacement.mock.calls, favorite.feedback(view())]).toEqual([[], null]);
});