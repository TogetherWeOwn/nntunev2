import { expect, test, vi } from 'vitest';
import { createCurrentTrackFavorite } from './currentTrackFavorite.svelte.js';
import { createQueue, DEMO_TRACKS, selectTrack } from './queue.js';
import { createQueueController } from './queueController.js';

function setup() {
	const initial = createQueue(DEMO_TRACKS);
	const queue = createQueueController(initial);
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

test('rejects a stale selection even after a round trip back to the same track', () => {
	const { favorite, select, view, write } = setup();
	const before = view();
	select(1);
	select(0);
	favorite.activate(before);
	expect(write).not.toHaveBeenCalled();
	expect(favorite.feedback(view())).toBeNull();
});

test('a refused owner cannot write or announce, and replacing it invalidates old intents', () => {
	const { favorite, view, write, refuse, dispose } = setup();
	const before = view();
	refuse();
	favorite.activate(before);
	expect(view()).toBeNull();
	expect(write).not.toHaveBeenCalled();
	const replacement = vi.fn(() => null);
	favorite.bind({ read: () => [], write: replacement });
	dispose();
	favorite.activate(before);
	expect(replacement).not.toHaveBeenCalled();
});

test('an auth purge retires both feedback and the ability to replay an earlier intent', () => {
	const { favorite, queue, view, write } = setup();
	const before = view();
	favorite.activate(before);
	queue.purgeAtAuthBoundary(() => {});
	favorite.activate(before);
	expect(view()).toBeNull();
	expect(favorite.feedback(view())).toBeNull();
	expect(write).toHaveBeenCalledTimes(1);
});
