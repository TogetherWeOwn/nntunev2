import { get } from 'svelte/store';
import { afterEach, expect, test, vi } from 'vitest';
import { clearMiniPlayerHint } from '../shell/mini-player.js';
import { createPlayerController } from './controller.svelte.js';
import { createQueue, currentTrack, selectTrack } from './queue.js';

const queue = createQueue(['first', 'second', 'third'].map((id) => ({ id, title: id, artist: '', streamUrl: `/${id}` })));

function setup() {
	vi.useFakeTimers();
	const flush = vi.fn(async () => { attach(currentTrack(player.queue.snapshot)!.streamUrl!); });
	const player = createPlayerController(queue, { flush }, (node, source) => node.getAttribute('src') === source);
	function attach(source: string) {
		const audio = Object.assign(new EventTarget(), {
			isConnected: true, paused: true, ended: false, error: null,
			getAttribute: () => source,
			play: vi.fn(async () => {
				audio.paused = audio.ended = false;
				audio.dispatchEvent(new Event('playing'));
			}),
			pause: vi.fn(() => {
				audio.paused = true;
				audio.dispatchEvent(new Event('pause'));
			}),
			end() {
				audio.paused = audio.ended = true;
				audio.dispatchEvent(new Event('pause'));
				audio.dispatchEvent(new Event('ended'));
			}
		});
		player.media.attach(audio as unknown as HTMLAudioElement);
		return audio;
	}
	const audio = attach('/first');
	const { sleepTimer, playCurrent, pauseCurrent } = player.media;
	return { player, audio, attach, flush, sleepTimer, playCurrent, pauseCurrent, state: () => get(sleepTimer) };
}

const duration = (ms: number) => ({ kind: 'duration' as const, ms });
const endOfTrack = { kind: 'end-of-track' as const };
afterEach(() => { vi.useRealTimers(); clearMiniPlayerHint(); });

test('duration expiry pauses the current owner once without changing selection', async () => {
	const { player, audio, sleepTimer, state, playCurrent } = setup();
	await playCurrent();
	sleepTimer.start(duration(1000), 'Sleep in 1 second');
	vi.advanceTimersByTime(500);
	expect(state()).toMatchObject({ armed: true, remaining: '0:01' });
	expect(audio.pause).not.toHaveBeenCalled();
	vi.advanceTimersByTime(5000);
	expect(audio.pause).toHaveBeenCalledOnce();
	expect(get(player.media.playbackStatus)).toBe('paused');
	expect(player.queue.snapshot).toBe(queue);
	expect(state()).toMatchObject({ armed: false, remaining: '', announcement: /fired/ });
	expect(vi.getTimerCount()).toBe(0);
});

test('cancel and rearm replace the deadline; duration runs while paused', async () => {
	const { audio, sleepTimer, state, playCurrent, pauseCurrent } = setup();
	sleepTimer.start(duration(500), 'old');
	sleepTimer.start(duration(1000), 'new');
	expect(vi.getTimerCount()).toBe(1);
	vi.advanceTimersByTime(500);
	expect(state()).toMatchObject({ armed: true, announcement: 'new' });
	sleepTimer.cancel();
	await playCurrent();
	vi.advanceTimersByTime(1000);
	expect(audio.paused).toBe(false);
	pauseCurrent();
	sleepTimer.start(duration(500), 'paused');
	vi.advanceTimersByTime(500);
	expect(state().announcement).toMatch(/fired/);
	expect(audio.pause).toHaveBeenCalledOnce();
	await playCurrent();
	expect(audio.paused).toBe(false);
});

test('a paused or retired decoder cannot consume a track end; a playable replacement inherits the timer', async () => {
	const { player, audio, state, playCurrent, attach, sleepTimer } = setup();
	sleepTimer.start(endOfTrack, 'Sleep at end of track');
	audio.end();
	expect(state().armed).toBe(true);
	player.replaceQueue(selectTrack(queue, 1));
	const next = attach('/second');
	await playCurrent();
	audio.end();
	expect(state().armed).toBe(true);
	player.media.setRepeat('one', null);
	next.end();
	await Promise.resolve();
	expect(state().announcement).toMatch(/fired/);
	expect(player.queue.snapshot.currentIndex).toBe(1);
	expect(next.play).toHaveBeenCalledOnce();
});

test('stop after two counts natural advance once and stops before the next queue change', async () => {
	const { player, audio, state, playCurrent, sleepTimer, flush } = setup();
	await playCurrent();
	sleepTimer.start({ kind: 'stop-after-tracks', count: 2 }, 'Sleep after 2 tracks');
	audio.end();
	audio.end();
	await Promise.resolve();
	expect(state()).toMatchObject({ armed: true, remaining: '1 track remaining' });
	expect(player.queue.snapshot.currentIndex).toBe(1);
	player.media.audio!.dispatchEvent(new Event('seeking'));
	expect(state().remaining).toBe('1 track remaining');
	(player.media.audio as unknown as typeof audio).end();
	expect(state().announcement).toMatch(/fired/);
	expect(player.queue.snapshot.currentIndex).toBe(1);
	expect(flush).toHaveBeenCalledOnce();
});

test('expiry during natural advance revokes autoplay before the replacement attaches', async () => {
	const { player, audio, sleepTimer, flush, attach, playCurrent } = setup();
	const pending = Promise.withResolvers<void>();
	flush.mockReturnValueOnce(pending.promise);
	await playCurrent();
	sleepTimer.start(duration(500), 'Sleep soon');
	audio.end();
	expect(player.queue.snapshot.currentIndex).toBe(1);
	expect(player.media.audio).toBeUndefined();
	vi.advanceTimersByTime(500);
	const next = attach('/second');
	pending.resolve();
	await Promise.resolve();
	expect(next.play).not.toHaveBeenCalled();
});

test.each(['source loss', 'auth purge', 'destroy'] as const)('%s clears timer state and scheduled work', (boundary) => {
	const { player, audio, state, sleepTimer } = setup();
	sleepTimer.start(duration(500), 'old session');
	if (boundary === 'source loss') player.replaceQueue(createQueue([]));
	else if (boundary === 'auth purge') player.purgeAtAuthBoundary();
	else player.destroy();
	expect(state()).toEqual({ armed: false, announcement: 'Sleep timer off', remaining: '' });
	expect(vi.getTimerCount()).toBe(0);
	vi.advanceTimersByTime(5000);
	expect(audio.pause).not.toHaveBeenCalled();
	sleepTimer.start(duration(500), 'stale control');
	expect(state().armed).toBe(false);
});
