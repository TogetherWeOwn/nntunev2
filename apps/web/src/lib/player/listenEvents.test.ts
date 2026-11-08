import { afterEach, describe, expect, it, vi } from 'vitest';
import {
	createListenEventSender,
	isValidTrackId,
	observeListenEvents,
	type ListenEvent
} from './listenEvents.js';

function fakeAudio() {
	const handlers = new Map<string, Set<() => void>>();
	return {
		ended: false,
		addEventListener: (n: string, l: () => void) => {
			if (!handlers.has(n)) handlers.set(n, new Set());
			handlers.get(n)!.add(l);
		},
		removeEventListener: (n: string, l: () => void) => handlers.get(n)?.delete(l),
		fire: (n: string) => [...(handlers.get(n) ?? [])].forEach((l) => l())
	};
}

function setup(track: string | undefined = 'track-a') {
	const sent: ListenEvent[] = [];
	const audio = fakeAudio();
	let n = 0;
	let currentTrack = track;
	const observer = observeListenEvents(audio, {
		trackId: () => currentTrack,
		sender: { send: (e) => void sent.push(e) },
		newId: () => `id-${++n}`
	});
	return { sent, audio, stop: observer.dispose, observer, setTrack: (t: string) => (currentTrack = t) };
}

describe('observeListenEvents', () => {
	it('emits start only on actual playing, once across pause/resume and buffering', () => {
		const { sent, audio } = setup();
		audio.fire('play');
		expect(sent).toEqual([]);
		audio.fire('playing');
		audio.fire('pause');
		audio.fire('playing');
		audio.fire('playing');
		expect(sent).toEqual([{ playback_id: 'id-1', track_id: 'track-a', event: 'start' }]);
	});

	it('completes on end and gives a repeat restart a fresh id', () => {
		const { sent, audio } = setup();
		audio.fire('playing');
		audio.fire('pause');
		audio.fire('ended');
		audio.fire('playing');
		expect(sent.map((e) => [e.playback_id, e.event])).toEqual([
			['id-1', 'start'], ['id-1', 'complete'], ['id-2', 'start']
		]);
	});

	it('skips a started playback on disposal with the captured track', () => {
		const { sent, stop, setTrack, audio } = setup();
		audio.fire('playing');
		setTrack('track-b');
		stop();
		stop();
		expect(sent.at(-1)).toEqual({ playback_id: 'id-1', track_id: 'track-a', event: 'skip' });
		expect(sent).toHaveLength(2);
	});

	it('skips when the source is emptied', () => {
		const { sent, audio } = setup();
		audio.fire('playing');
		audio.fire('emptied');
		audio.fire('emptied');
		expect(sent.map((e) => e.event)).toEqual(['start', 'skip']);
	});

	it('completes, not skips, when teardown races ahead of the ended listener', () => {
		const { sent, audio, stop } = setup();
		audio.fire('playing');
		audio.ended = true;
		stop();
		expect(sent.map((e) => e.event)).toEqual(['start', 'complete']);
	});

	it('remembers a natural end sampled before ended was cleared by teardown', () => {
		const { sent, audio } = setup();
		audio.fire('playing');
		audio.ended = true;
		audio.fire('pause');
		audio.ended = false;
		audio.fire('emptied');
		expect(sent.map((e) => e.event)).toEqual(['start', 'complete']);
	});

	it('a seek away from the end is not a completion', () => {
		const { sent, audio, stop } = setup();
		audio.fire('playing');
		audio.ended = true;
		audio.fire('pause');
		audio.ended = false;
		audio.fire('seeking');
		stop();
		expect(sent.map((e) => e.event)).toEqual(['start', 'skip']);
	});

	it('reset at an auth boundary forgets the playback without a terminal', () => {
		const { sent, audio, observer, stop } = setup();
		audio.fire('playing');
		observer.reset();
		audio.fire('ended');
		stop();
		expect(sent.map((e) => e.event)).toEqual(['start']);
	});

	it('contains id source and sender failures', () => {
		const audio = fakeAudio();
		observeListenEvents(audio, {
			trackId: () => 'a',
			sender: { send: () => { throw new Error('boom'); } },
			newId: () => { throw new Error('no uuid'); }
		});
		expect(() => audio.fire('playing')).not.toThrow();
		const audio2 = fakeAudio();
		const o = observeListenEvents(audio2, {
			trackId: () => 'a',
			sender: { send: () => { throw new Error('boom'); } }
		});
		expect(() => audio2.fire('playing')).not.toThrow();
		expect(() => o.dispose()).not.toThrow();
	});

	it('emits nothing for a playback that never started', () => {
		const { sent, audio, stop } = setup();
		audio.fire('play');
		audio.fire('error');
		stop();
		expect(sent).toEqual([]);
	});

	it('does not start without a track id', () => {
		const { sent, audio } = setup(undefined);
		audio.fire('playing');
		expect(sent).toEqual([]);
	});
});

describe('createListenEventSender', () => {
	afterEach(() => vi.useRealTimers());
	const ev = (event: ListenEvent['event']): ListenEvent => ({
		playback_id: '11111111-1111-4111-8111-111111111111', track_id: 't', event
	});
	const ok = () => new Response('{}', { status: 200 });

	it('posts credentialed JSON with only the three fields, start before terminal', async () => {
		const calls: { url: string; init: RequestInit }[] = [];
		let release = () => {};
		const gate = new Promise<void>((r) => (release = r));
		const fetchFn = vi.fn(async (url: string | URL | Request, init?: RequestInit) => {
			calls.push({ url: String(url), init: init! });
			if (calls.length === 1) await gate;
			return ok();
		});
		const sender = createListenEventSender({ baseUrl: 'https://api.test/', fetchFn });
		sender.send(ev('start'));
		sender.send(ev('complete'));
		await vi.waitFor(() => expect(calls).toHaveLength(1));
		release();
		await vi.waitFor(() => expect(calls).toHaveLength(2));
		expect(calls.map((c) => JSON.parse(String(c.init.body)).event)).toEqual(['start', 'complete']);
		expect(calls[0].url).toBe('https://api.test/v1/plays/events');
		expect(calls[0].init.credentials).toBe('include');
		expect(Object.keys(JSON.parse(String(calls[0].init.body))).sort()).toEqual([
			'event', 'playback_id', 'track_id'
		]);
	});

	it('retries transient failures a bounded number of times with the same payload', async () => {
		const fetchFn = vi.fn(async () => new Response('', { status: 503 }));
		const sender = createListenEventSender({ baseUrl: '', fetchFn, retryDelaysMs: [1, 1] });
		sender.send(ev('start'));
		await vi.waitFor(() => expect(fetchFn).toHaveBeenCalledTimes(3));
		await new Promise((r) => setTimeout(r, 20));
		expect(fetchFn).toHaveBeenCalledTimes(3);
		const bodies = fetchFn.mock.calls.map((c) => (c as unknown as [string, RequestInit])[1].body);
		expect(new Set(bodies).size).toBe(1);
	});

	it('retries network errors but not other 4xx', async () => {
		const net = vi.fn().mockRejectedValueOnce(new TypeError('x')).mockResolvedValue(ok());
		createListenEventSender({ baseUrl: '', fetchFn: net, retryDelaysMs: [1] }).send(ev('start'));
		await vi.waitFor(() => expect(net).toHaveBeenCalledTimes(2));
		const bad = vi.fn(async () => new Response('', { status: 400 }));
		createListenEventSender({ baseUrl: '', fetchFn: bad, retryDelaysMs: [1] }).send(ev('start'));
		await new Promise((r) => setTimeout(r, 20));
		expect(bad).toHaveBeenCalledTimes(1);
	});

	it('stops all telemetry after a 401 and never throws', async () => {
		const fetchFn = vi.fn(async () => new Response('', { status: 401 }));
		const sender = createListenEventSender({ baseUrl: '', fetchFn, retryDelaysMs: [1] });
		sender.send(ev('start'));
		await vi.waitFor(() => expect(fetchFn).toHaveBeenCalledTimes(1));
		await new Promise((r) => setTimeout(r, 10));
		expect(() => sender.send(ev('complete'))).not.toThrow();
		await new Promise((r) => setTimeout(r, 10));
		expect(fetchFn).toHaveBeenCalledTimes(1);
	});

	it('reset drops pending retries so nothing uploads for a new account', async () => {
		const fetchFn = vi.fn(async () => new Response('', { status: 500 }));
		const sender = createListenEventSender({ baseUrl: '', fetchFn, retryDelaysMs: [30] });
		sender.send(ev('start'));
		await vi.waitFor(() => expect(fetchFn).toHaveBeenCalledTimes(1));
		sender.reset();
		await new Promise((r) => setTimeout(r, 60));
		expect(fetchFn).toHaveBeenCalledTimes(1);
	});

	it('rejects control characters in track ids', () => {
		for (const id of ['a\nb', 'a\u0000', '\u007f', 'x\u0085y', '']) {
			expect(isValidTrackId(id)).toBe(false);
		}
		expect(isValidTrackId('ok-id:1')).toBe(true);
	});

	it('honours Retry-After on 429 and caps it', async () => {
		vi.useFakeTimers();
		const fetchFn = vi
			.fn()
			.mockResolvedValueOnce(new Response('', { status: 429, headers: { 'retry-after': '60' } }))
			.mockResolvedValue(ok());
		createListenEventSender({ baseUrl: '', fetchFn, retryDelaysMs: [1] }).send(ev('start'));
		await vi.advanceTimersByTimeAsync(30_000);
		expect(fetchFn).toHaveBeenCalledTimes(1);
		await vi.advanceTimersByTimeAsync(31_000);
		expect(fetchFn).toHaveBeenCalledTimes(2);
	});

	it('does not send a terminal when the start was rejected (422) or dropped', async () => {
		const fetchFn = vi.fn(async () => new Response('', { status: 422 }));
		const sender = createListenEventSender({ baseUrl: '', fetchFn, retryDelaysMs: [1] });
		sender.send(ev('start'));
		sender.send(ev('complete'));
		await new Promise((r) => setTimeout(r, 30));
		expect(fetchFn).toHaveBeenCalledTimes(1);
		// A terminal with no known start is never sent.
		sender.send({ ...ev('skip'), playback_id: '22222222-2222-4222-8222-222222222222' });
		await new Promise((r) => setTimeout(r, 10));
		expect(fetchFn).toHaveBeenCalledTimes(1);
	});

	it('drops an over-long track id', async () => {
		const fetchFn = vi.fn(async () => ok());
		createListenEventSender({ baseUrl: '', fetchFn }).send({ ...ev('start'), track_id: 'é'.repeat(65) });
		await new Promise((r) => setTimeout(r, 10));
		expect(fetchFn).not.toHaveBeenCalled();
	});
});
