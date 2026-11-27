import { expect, test, vi } from 'vitest';
import { setup } from './fixtures/browserPlayback.js';

function simulateConnectivity() {
	const network = new EventTarget();
	let online = true;
	vi.stubGlobal('addEventListener', network.addEventListener.bind(network));
	vi.stubGlobal('removeEventListener', network.removeEventListener.bind(network));
	Object.defineProperty(globalThis.navigator, 'onLine', { configurable: true, get: () => online });
	return {
		drop() { online = false; },
		restore() { online = true; network.dispatchEvent(new Event('online')); },
		cleanup() { Reflect.deleteProperty(globalThis.navigator, 'onLine'); vi.unstubAllGlobals(); }
	};
}

test('a private handoff that loses the network keeps its successor and retries that grant once on reconnect', async () => {
	const network = simulateConnectivity();
	try {
		const { player, media, queue, fetchFn, src, attach, toggleDuringFirstPlay, requests } = setup();
		await toggleDuringFirstPlay();
		const first = src(), tab = first.split('/')[5];
		expect(player.privateListening?.enabled).toBe(true);
		const ended = attach(false);
		network.drop();
		fetchFn.mockRejectedValueOnce(new TypeError('Failed to fetch'));
		ended.finish();
		await vi.waitFor(() => expect(requests()).toHaveLength(3));
		await vi.waitFor(() => expect(player.queueAnnouncement.text).toBe('Could not start the next track. Choose a track to continue.'));
		// The ended track stays selected and its play is not reopened while offline.
		expect([queue.snapshot.currentIndex, src(), media.audio]).toEqual([0, first, ended]);

		network.restore();
		await vi.waitFor(() => expect(src()).toBe(`/v1/plays/media/second/${tab}/play-2`));
		const successor = media.audio as unknown as { currentTime: number; paused: boolean };
		expect([queue.snapshot.currentIndex, successor === (ended as unknown), successor.currentTime, successor.paused])
			.toEqual([1, false, 0, false]);

		// A later reconnect cannot start another grant or reopen the previous track's play.
		network.drop();
		network.restore();
		await Promise.resolve();
		expect(requests().slice(2)).toEqual(Array(2).fill({ track_id: 'second', session_id: tab }));
		expect(requests().filter((body) => body.track_id === 'first')).toHaveLength(1);
	} finally {
		network.cleanup();
	}
});

test('replaying the ended track before reconnect cancels the pending successor retry', async () => {
	const network = simulateConnectivity();
	try {
		const { player, media, queue, fetchFn, src, attach, toggleDuringFirstPlay, requests } = setup();
		await toggleDuringFirstPlay();
		const first = src();
		const ended = attach(false);
		network.drop();
		fetchFn.mockRejectedValueOnce(new TypeError('Failed to fetch'));
		ended.finish();
		await vi.waitFor(() => expect(player.queueAnnouncement.text).toBe('Could not start the next track. Choose a track to continue.'));
		await media.playCurrent();
		expect(ended.play).toHaveBeenCalledOnce();

		network.restore();
		await Promise.resolve();
		expect([requests().length, queue.snapshot.currentIndex, src(), media.audio]).toEqual([3, 0, first, ended]);
	} finally {
		network.cleanup();
	}
});

test('native replay before reconnect cancels the pending successor retry', async () => {
	const network = simulateConnectivity();
	try {
		const { player, queue, fetchFn, src, attach, toggleDuringFirstPlay, requests } = setup();
		await toggleDuringFirstPlay();
		const first = src();
		const ended = attach(false);
		network.drop();
		fetchFn.mockRejectedValueOnce(new TypeError('Failed to fetch'));
		ended.finish();
		await vi.waitFor(() => expect(player.queueAnnouncement.text).toBe('Could not start the next track. Choose a track to continue.'));
		ended.ended = false;
		ended.paused = false;
		ended.dispatchEvent(new Event('play'));

		network.restore();
		await Promise.resolve();
		expect([requests().length, queue.snapshot.currentIndex, src()]).toEqual([3, 0, first]);
	} finally {
		network.cleanup();
	}
});

test('a mid-request network drop during next-track grant retries once on reconnect', async () => {
	const network = simulateConnectivity();
	try {
		const { player, media, queue, fetchFn, src, attach, toggleDuringFirstPlay, requests } = setup();
		await toggleDuringFirstPlay();
		const first = src(), tab = first.split('/')[5];
		const ended = attach(false);
		const started = Promise.withResolvers<void>(), fail = Promise.withResolvers<never>();
		fetchFn.mockImplementationOnce(async () => {
			started.resolve();
			return fail.promise;
		});
		ended.finish();
		await started.promise;
		network.drop();
		fail.reject(new TypeError('Failed to fetch'));
		await vi.waitFor(() => expect(player.queueAnnouncement.text).toBe('Could not start the next track. Choose a track to continue.'));
		expect([queue.snapshot.currentIndex, src(), media.audio]).toEqual([0, first, ended]);

		network.restore();
		await vi.waitFor(() => expect(src()).toBe(`/v1/plays/media/second/${tab}/play-2`));
		const successor = media.audio as unknown as { currentTime: number; paused: boolean };
		expect([queue.snapshot.currentIndex, successor === (ended as unknown), successor.currentTime, successor.paused])
			.toEqual([1, false, 0, false]);
		expect(requests().filter((body) => body.track_id === 'first')).toHaveLength(1);
		expect(requests().slice(2)).toEqual(Array(2).fill({ track_id: 'second', session_id: tab }));
	} finally {
		network.cleanup();
	}
});

test('native replay during a stalled online grant prevents a late reconnect retry after mid-request drop', async () => {
	const network = simulateConnectivity();
	try {
		const { player, media, queue, fetchFn, src, attach, toggleDuringFirstPlay, requests } = setup();
		await toggleDuringFirstPlay();
		const first = src();
		const ended = attach(false);
		const started = Promise.withResolvers<void>(), fail = Promise.withResolvers<never>();
		fetchFn.mockImplementationOnce(async () => {
			started.resolve();
			return fail.promise;
		});
		ended.finish();
		await started.promise;
		ended.ended = false;
		ended.paused = false;
		ended.dispatchEvent(new Event('play'));
		network.drop();
		fail.reject(new TypeError('Failed to fetch'));
		await vi.waitFor(() => expect(player.queueAnnouncement.text).toBe('Could not start the next track. Choose a track to continue.'));

		network.restore();
		await Promise.resolve();
		expect([requests().length, queue.snapshot.currentIndex, src(), media.audio, ended.ended, ended.paused])
			.toEqual([3, 0, first, ended, false, false]);
	} finally {
		network.cleanup();
	}
});
