import { expect, test, vi } from 'vitest';
import { createCurrentTrackFavorite } from './currentTrackFavorite.svelte.js';
import { createQueue, DEMO_TRACKS, selectTrack } from './queue.js';
import { createQueueController } from './queueController.js';

function setup() {
	const queue = createQueueController(createQueue(DEMO_TRACKS));
	const favorite = createCurrentTrackFavorite(queue);
	let ids: string[] = [];
	let allowed = true;
	const write = vi.fn((id: string, pressed: boolean) => {
		ids = pressed ? [...ids, id] : ids.filter((item) => item !== id);
		return 'saved' as const;
	});
	const dispose = favorite.bind({ read: () => allowed ? ids : null, write });
	const select = (index: number) => {
		const next = selectTrack(queue.snapshot, index);
		queue.publish(next, queue.begin(next, 'select'));
	};
	const view = () => favorite.view(queue.snapshot)!;
	return { favorite, queue, view, write, select, dispose, refuse: () => { allowed = false; } };
}

test('writes just the selected track, once per rendered intent, and reports the applied state', () => {
	const { favorite, select, view, write } = setup();
	select(1);
	const before = view();
	favorite.activate(before);
	favorite.activate(before);
	expect(write.mock.calls).toEqual([['second-signal', true]]);
	expect(favorite.feedback(view())?.text).toBe('Starred Second Signal.');
	favorite.activate(view());
	expect(write.mock.calls).toEqual([['second-signal', true], ['second-signal', false]]);
	expect(favorite.feedback(view())?.text).toBe('Unstarred Second Signal.');
});

test.each(['selection round trip', 'owner refusal'] as const)('%s cannot write or announce', (change) => {
	const { favorite, select, view, write, refuse } = setup();
	const before = view();
	if (change === 'selection round trip') { select(1); select(0); }
	else refuse();
	favorite.activate(before);
	expect(write).not.toHaveBeenCalled();
	expect(favorite.feedback(view())).toBeNull();
});

test('disposing the live owner prevents replay before and after replacement', () => {
	const { favorite, view, write, dispose } = setup();
	const before = view();
	dispose();
	favorite.activate(before);
	expect([view(), favorite.feedback(view())]).toEqual([null, null]);
	expect(write).not.toHaveBeenCalled();
	const replacement = vi.fn(() => null);
	favorite.bind({ read: () => before.membership, write: replacement });
	favorite.activate(before);
	expect(replacement).not.toHaveBeenCalled();
	expect(favorite.feedback(view())).toBeNull();
});