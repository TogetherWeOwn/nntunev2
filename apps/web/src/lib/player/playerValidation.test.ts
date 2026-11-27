import { writable } from 'svelte/store';
import { expect, test, vi } from 'vitest';
import { handleAppShortcut } from './appShortcuts.js';
import { ARTWORK_PLACEHOLDER_SRC, nextArtworkSrcAfterError, resolveArtwork } from './artwork.js';
import { createCurrentTrackFavorite } from './currentTrackFavorite.svelte.js';
import { createPlayerListenHistory } from './playerListenHistory.js';
import { createQueue, DEMO_TRACKS } from './queue.js';
import { createQueueController } from './queueController.js';
import { matchesQueueQuery } from './queueFilter.js';
import { createQueueRevision } from './queueRevision.js';

test.each(['retrying', 'blocked'])('playback shortcut honors the %s transport state', (status) => {
	const media = { playbackStatus: writable(status), playCurrent: vi.fn(), pauseCurrent: vi.fn() };
	const event = {
		key: 'k', preventDefault: vi.fn(),
		view: { document: { querySelector: () => null, querySelectorAll: () => [] } }
	};
	handleAppShortcut(event as unknown as KeyboardEvent,
		media as unknown as Parameters<typeof handleAppShortcut>[1],
		{ isOpen: () => false } as Parameters<typeof handleAppShortcut>[2]);
	expect([media.playCurrent.mock.calls.length, event.preventDefault.mock.calls.length])
		.toEqual(status === 'blocked' ? [1, 1] : [0, 0]);
});

test('artwork rejects malformed and ambiguous URLs, including invalid fallbacks', () => {
	for (const source of ['//host/art', '/\\host/art', 'ht\ttps://host/art', 'http:///host',
		'https://name:pass@host/art', 'https://[', 'data:text/html,hello', 'data:image/png,', 'blob:bad']) {
		expect(resolveArtwork(source, { fallbackSrc: source }).src).toBe(ARTWORK_PLACEHOLDER_SRC);
	}
	expect(nextArtworkSrcAfterError('/failed', 'javascript:refused')).toBeNull();
	expect(nextArtworkSrcAfterError('/failed', ' /fallback ')).toBe('/fallback');
});

test('favorite drops a read revoked mid-call and tolerates a rejected owner write', async () => {
	const snapshot = createQueue(DEMO_TRACKS);
	const queue = createQueueController(snapshot);
	const membership = { pressed: false, edit: 0, loading: false };
	const read = vi.fn(() => membership).mockImplementationOnce(() => { queue.revoke(); return membership; });
	const write = vi.fn(async () => { throw new Error('Write refused'); });
	const favorite = createCurrentTrackFavorite(queue);
	favorite.bind({ read, write });
	expect(favorite.view(snapshot)).toBeNull();
	const selection = favorite.view(snapshot);
	if (!selection) throw new Error('Expected a current favorite');
	await favorite.activate(selection);
	expect(write).toHaveBeenCalledWith(selection.id);
	expect(favorite.feedback(selection)).toBeNull();
});

test('listen history rejects malformed ids, caps starts and still finishes admitted listens', () => {
	let starts = 0, terminals = 0, id = 0;
	const history = createPlayerListenHistory({
		send: ({ event }) => { if (event === 'start') starts++; else terminals++; },
		close: async () => {}, dispose: () => {}
	}, new EventTarget() as Window);
	const audio = Object.assign(new EventTarget(), { paused: false, seeking: false, ended: false, error: null }) as HTMLAudioElement;
	const grant = () => id.toString(36).padStart(4, '0').padEnd(22, 'A');
	try {
		for (const invalid of [' ', 'track\n', 'é'.repeat(65)]) history.attach(audio, invalid, grant);
		audio.dispatchEvent(new Event('playing'));
		expect(starts).toBe(0);
		history.attach(audio, 'é'.repeat(64), grant);
		for (; id <= 16_384; id++) {
			audio.dispatchEvent(new Event('playing'));
			audio.dispatchEvent(new Event('emptied'));
		}
		expect([starts, terminals]).toEqual([16_384, 16_384]);
	} finally {
		history.resetAtAuthBoundary();
	}
});

test('queue filter applies one bound to normalized queries and preserves mixed matches', () => {
	const track = { id: 'part name', title: '', artist: 'Café' };
	expect(matchesQueueQuery(track, 'name Cafe\u0301')).toBe(true);
	expect(matchesQueueQuery(track, ' '.repeat(4096) + 'é')).toBe(true);
	const longTitle = 'a'.repeat(4097);
	expect(matchesQueueQuery({ ...track, title: longTitle }, longTitle)).toBe(false);
	expect(matchesQueueQuery({ ...track, artist: 5 as unknown as string }, 'name')).toBe(false);
});

test('queue revision stays retired after repeated destroy and revoke calls', () => {
	const queue = createQueueRevision();
	const next = queue.revoke();
	expect([queue.isCurrent(0), queue.isCurrent(next)]).toEqual([false, true]);
	for (let call = 0; call < 2; call++) queue.destroy();
	expect([queue.revoke(), queue.isCurrent(next)]).toEqual([next, false]);
});
