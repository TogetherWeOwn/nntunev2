import { afterEach, describe, expect, it, vi } from 'vitest';
import { createListenEventSender, isValidTrackId, type ListenEvent } from './listenEventSender.js';

describe('createListenEventSender', () => {
	afterEach(() => vi.useRealTimers());
	const ev = (event: ListenEvent['event']): ListenEvent => ({
		playback_id: '11111111-1111-4111-8111-111111111111',
		track_id: 't',
		event
	});
	const context = { current: () => 'a'.repeat(43) };
	const ok = () => new Response('{}', { status: 200 });

	it('posts credentialed JSON with the binding, start before terminal', async () => {
		const calls: { url: string; init: RequestInit }[] = [];
		let release = () => {};
		const gate = new Promise<void>((r) => (release = r));
		const fetchFn = vi.fn(async (url: string | URL | Request, init?: RequestInit) => {
			calls.push({ url: String(url), init: init! });
			if (calls.length === 1) await gate;
			return ok();
		});
		const sender = createListenEventSender({ context, baseUrl: 'https://api.test/', fetchFn });
		sender.send(ev('start'));
		sender.send(ev('complete'));
		await vi.waitFor(() => expect(calls).toHaveLength(1));
		release();
		await vi.waitFor(() => expect(calls).toHaveLength(2));
		expect(calls.map((c) => JSON.parse(String(c.init.body)).event)).toEqual(['start', 'complete']);
		expect(calls[0].url).toBe('https://api.test/v1/plays/events');
		expect(calls[0].init.credentials).toBe('include');
		expect(Object.keys(JSON.parse(String(calls[0].init.body))).sort()).toEqual([
			'context',
			'event',
			'playback_id',
			'track_id'
		]);
	});

	it('keeps the start context on retries and the terminal, without a reset signal', async () => {
		vi.useFakeTimers();
		const first = 'a'.repeat(43);
		let current = first;
		const bindings: string[] = [];
		const fetchFn: typeof fetch = async (_url, init) => {
			bindings.push(JSON.parse(String(init?.body)).context);
			return bindings.length === 1 ? new Response('', { status: 503 }) : ok();
		};
		const sender = createListenEventSender({
			context: { current: () => current },
			baseUrl: '',
			fetchFn,
			retryDelaysMs: [30]
		});
		sender.send(ev('start'));
		await vi.advanceTimersByTimeAsync(0);
		expect(bindings).toEqual([first]);

		current = 'b'.repeat(43);
		sender.send(ev('complete'));
		await vi.advanceTimersByTimeAsync(30);
		expect(bindings).toEqual([first, first, first]);
	});

	it('does not send an earlier playback when context becomes available later', async () => {
		const fetchFn = vi.fn<typeof fetch>(async () => ok());
		let current: string | undefined;
		const sender = createListenEventSender({
			context: { current: () => current },
			baseUrl: '',
			fetchFn
		});
		sender.send(ev('start'));
		current = 'a'.repeat(43);
		sender.send(ev('complete'));
		await Promise.resolve();
		expect(fetchFn).not.toHaveBeenCalled();
	});

	it('retries transient failures a bounded number of times with the same payload', async () => {
		const fetchFn = vi.fn(async () => new Response('', { status: 503 }));
		const sender = createListenEventSender({ context, baseUrl: '', fetchFn, retryDelaysMs: [1, 1] });
		sender.send(ev('start'));
		await vi.waitFor(() => expect(fetchFn).toHaveBeenCalledTimes(3));
		await new Promise((r) => setTimeout(r, 20));
		expect(fetchFn).toHaveBeenCalledTimes(3);
		const bodies = fetchFn.mock.calls.map((c) => (c as unknown as [string, RequestInit])[1].body);
		expect(new Set(bodies).size).toBe(1);
	});

	it('retries network errors but not other 4xx', async () => {
		const net = vi.fn().mockRejectedValueOnce(new TypeError('x')).mockResolvedValue(ok());
		createListenEventSender({ context, baseUrl: '', fetchFn: net, retryDelaysMs: [1] }).send(ev('start'));
		await vi.waitFor(() => expect(net).toHaveBeenCalledTimes(2));
		const bad = vi.fn(async () => new Response('', { status: 400 }));
		createListenEventSender({ context, baseUrl: '', fetchFn: bad, retryDelaysMs: [1] }).send(ev('start'));
		await new Promise((r) => setTimeout(r, 20));
		expect(bad).toHaveBeenCalledTimes(1);
	});

	it('stops all telemetry after a 401 and never throws', async () => {
		const fetchFn = vi.fn(async () => new Response('', { status: 401 }));
		const sender = createListenEventSender({ context, baseUrl: '', fetchFn, retryDelaysMs: [1] });
		sender.send(ev('start'));
		await vi.waitFor(() => expect(fetchFn).toHaveBeenCalledTimes(1));
		await new Promise((r) => setTimeout(r, 10));
		expect(() => sender.send(ev('complete'))).not.toThrow();
		await new Promise((r) => setTimeout(r, 10));
		expect(fetchFn).toHaveBeenCalledTimes(1);
	});

	it('reset drops pending retries so nothing uploads for a new account', async () => {
		const fetchFn = vi.fn(async () => new Response('', { status: 500 }));
		vi.useFakeTimers();
		const sender = createListenEventSender({ context, baseUrl: '', fetchFn, retryDelaysMs: [30] });
		sender.send(ev('start'));
		// Flush microtasks only: the first attempt runs, the retry timer has not fired.
		await vi.advanceTimersByTimeAsync(0);
		expect(fetchFn).toHaveBeenCalledTimes(1);
		sender.reset();
		await vi.advanceTimersByTimeAsync(1000);
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
		createListenEventSender({ context, baseUrl: '', fetchFn, retryDelaysMs: [1] }).send(ev('start'));
		await vi.advanceTimersByTimeAsync(30_000);
		expect(fetchFn).toHaveBeenCalledTimes(1);
		await vi.advanceTimersByTimeAsync(31_000);
		expect(fetchFn).toHaveBeenCalledTimes(2);
	});

	it('does not send a terminal when the start was rejected (422) or dropped', async () => {
		const fetchFn = vi.fn(async () => new Response('', { status: 422 }));
		const sender = createListenEventSender({ context, baseUrl: '', fetchFn, retryDelaysMs: [1] });
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
		createListenEventSender({ context, baseUrl: '', fetchFn }).send({ ...ev('start'), track_id: 'é'.repeat(65) });
		await new Promise((r) => setTimeout(r, 10));
		expect(fetchFn).not.toHaveBeenCalled();
	});
});
