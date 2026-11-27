import { expect, test, vi } from 'vitest';
import { readAbsoluteSeek, writeAbsoluteSeek } from './absoluteSeek.js';
import { cacheControlFor, type MediaKind } from './cacheHeaders.js';
import { createLivePlaybackSpeed } from './livePlaybackSpeed.js';
import { createPendingShareSeek } from './pendingShareSeek.js';
import { createPlayback } from './playback.js';
import { parsePlaybackSpeed } from './playbackSpeed.js';
import { createQueue, decodeQueue, DEMO_TRACKS, serializeQueue, shuffleUpcoming } from './queue.js';
import { createQueueShare } from './queueShare.js';
import { DEFAULT_RADIO_SEED, parseRadioSeed } from './radio.js';
import { createRecentlyPlayedStore } from './recentlyPlayed.js';
import { jumpMedia } from './relativeJump.js';
import { resolveTrackLinearGain } from './replayGain.js';
import { restartIsAvailable } from './restartAvailability.js';
import { createResumeStore } from './resume.js';
import { SeekController } from './seekController.js';
import { buildShareQuery, parseTimestampParam } from './shareTimestamp.js';

type TestMedia = Pick<HTMLAudioElement, 'currentTime' | 'duration' | 'seeking' | 'error' | 'seekable'>;

function media(start = 0, end = 100): TestMedia {
	return { currentTime: 10, duration: 100, seeking: false, error: null,
		seekable: { length: 1, start: () => start, end: () => end } };
}

test('an unknown cache kind cannot grant public caching', () => {
	expect(cacheControlFor('unknown' as MediaKind)).toBe('no-store');
});

test('timestamp parsing and building enforce the same numeric range', () => {
	expect(parseTimestampParam(`${'9'.repeat(310)}:00`)).toBeNull();
	expect(parseTimestampParam('9007199254740992')).toBeNull();
	expect(() => buildShareQuery('a', 1e21)).toThrow(RangeError);
	expect(parseTimestampParam('1:02:03')).toBe(3723);
});

test('invalid seek arguments and unusable ranges never authorize native seeking', () => {
	const audio = media();
	expect(writeAbsoluteSeek(audio, Infinity, () => true)).not.toBe('');
	expect(audio.currentTime).toBe(10);
	expect(jumpMedia(audio, 'forward', undefined, undefined, -3).kind).toBe('unavailable');
	expect(audio.currentTime).toBe(10);
	expect(readAbsoluteSeek(media(0, Infinity)).enabled).toBe(false);
	expect(jumpMedia(media(20, 20), 'forward').kind).toBe('unavailable');
	expect(jumpMedia({ ...media(), error: {} as MediaError }, 'forward').kind).toBe('unavailable');
	expect(restartIsAvailable(media(0, 0) as HTMLAudioElement)).toBe(false);
});

test('an invalid pending share seek does not reach resume or native media', () => {
	const seek = createPendingShareSeek();
	const audio = media() as HTMLAudioElement;
	const cancelResume = vi.fn();
	seek.set({ trackId: 'a', streamUrl: '/a', seconds: -5 }, () => ({ isCurrent: () => true, cancelResume }));
	seek.apply(audio);
	expect(cancelResume).not.toHaveBeenCalled();
	expect(audio.currentTime).toBe(10);
	expect(seek.current).toBeNull();
});

test.each(['success', 'failure'])('late play %s is ignored after a same-element source swap', async (outcome) => {
	let source = '/a';
	const play = vi.fn(async () => {
		source = '/b';
		if (outcome === 'failure') throw new Error('retired source failed');
	});
	const element = { play, pause: vi.fn(), load: vi.fn(), getAttribute: () => source };
	const onPlaybackStarted = vi.fn();
	const onPlaybackFailure = vi.fn();
	const playback = createPlayback({ audio: () => element, flush: async () => {}, onPlaybackStarted, onPlaybackFailure });
	await playback.play();
	expect(play).toHaveBeenCalledOnce();
	expect(onPlaybackStarted).not.toHaveBeenCalled();
	expect(onPlaybackFailure).not.toHaveBeenCalled();
});

test('malformed stored speed and non-finite handoff cannot reach the decoder', () => {
	expect(parsePlaybackSpeed('0x2')).toBe(1);
	const audio = { playbackRate: 1 };
	const live = createLivePlaybackSpeed(() => ({ audio, matches: () => true }));
	live.restore(null);
	live.handoff(() => Infinity);
	live.attach();
	expect(audio.playbackRate).toBe(1);
});

test('an invalid random sample cannot corrupt the original queue or produce a replacement', () => {
	const queue = createQueue(DEMO_TRACKS);
	expect(shuffleUpcoming(queue, () => 1)).toBe(queue);
	expect(queue.tracks).toEqual(DEMO_TRACKS);
	expect(decodeQueue(serializeQueue(queue).padEnd(65_537), DEMO_TRACKS)).toBeNull();
});

test('disposal during the copy reset prevents clipboard access', async () => {
	const queue = createQueue(DEMO_TRACKS);
	const writeText = vi.fn(async () => {});
	const share = createQueueShare({
		getQueue: () => queue, getPosition: () => 10, getOrigin: () => 'https://example.test',
		getClipboard: () => ({ writeText }), shortTitle: (title) => title,
		onOutcome: () => share.dispose()
	});
	await share.copy(DEMO_TRACKS[0]!.id);
	expect(writeText).not.toHaveBeenCalled();
});

test('radio seeds reject trailing text and ReplayGain rejects unusable amplitudes', () => {
	expect(parseRadioSeed('8trailing')).toBe(DEFAULT_RADIO_SEED);
	expect(resolveTrackLinearGain({ trackGainDb: 900 }, { enabled: true })).toBe(1);
	expect(resolveTrackLinearGain({ trackGainDb: -10_000 }, { enabled: true })).toBe(1);
});

test('a temporary storage read failure does not overwrite resume or recent history', () => {
	const getItem = vi.fn(() => { throw Error('unavailable'); });
	const storage = { getItem, setItem: vi.fn(), removeItem: vi.fn() };
	createResumeStore({ storage }).save('new', 10);
	createRecentlyPlayedStore({ storage }).record('new', 10);
	expect(storage.setItem).not.toHaveBeenCalled();
});

test('an invalid replay clock leaves the saved entry intact', () => {
	const storage = { getItem: () => '[{"trackId":"a","playedAt":10}]', setItem: vi.fn(), removeItem: vi.fn() };
	const recent = createRecentlyPlayedStore({ storage, now: () => NaN });
	recent.replay('a');
	expect(recent.entries()).toEqual([{ trackId: 'a', playedAt: 10 }]);
	expect(storage.setItem).not.toHaveBeenCalled();
});

test('a seek deadline settles before late headers and releases the late body', async () => {
	expect(() => new SeekController('/audio', undefined, { deadlineMs: 2 ** 31 })).toThrow(RangeError);
	vi.useFakeTimers();
	try {
		let respond!: (response: Response) => void;
		const seek = new SeekController('/audio', () => new Promise((resolve) => { respond = resolve; }), { deadlineMs: 10 });
		const result = seek.seek(0, 0).catch((error: unknown) => error);
		await vi.advanceTimersByTimeAsync(10);
		expect(seek.pending).toBe(false);
		expect(await result).toMatchObject({ name: 'TimeoutError' });
		const cancel = vi.fn();
		respond(new Response(new ReadableStream({ cancel }), { status: 500 }));
		await Promise.resolve();
		expect(cancel).toHaveBeenCalledOnce();
	} finally {
		vi.useRealTimers();
	}
});

test.each([
	[500, 'Range request failed with HTTP 500'],
	[206, 'Range response body length does not match Content-Range']
] as const)('a rejected HTTP %i seek does not wait for upstream cancellation', async (status, message) => {
	const cancel = vi.fn(() => new Promise<void>(() => {}));
	const stream = new ReadableStream({ start: (body) => body.enqueue(new Uint8Array(2)), cancel });
	const seek = new SeekController('/audio', async () =>
		new Response(stream, { status, headers: { 'content-range': 'bytes 0-0/1' } }));
	const result = seek.seek(0).catch((error: unknown) => error);
	await vi.waitFor(() => expect(seek.pending).toBe(false), { timeout: 200 });
	expect(await result).toMatchObject({ message });
	expect(cancel).toHaveBeenCalledOnce();
	expect(stream.locked).toBe(false);
});
