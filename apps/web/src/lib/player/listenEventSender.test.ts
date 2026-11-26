import { afterEach, expect, it, onTestFinished, vi } from 'vitest';
import type { AccountSession } from '../auth-fence.js';
import { createBrowserPlaybackGrant, type BrowserPlaybackSource } from './browserPlaybackGrant.js';
import { createListenEventSender } from './listenEventSender.js';
import type { ListenEvent } from './observeListenEvents.js';

const binding = 'A'.repeat(43);
const context = 'B'.repeat(42) + 'E';
// The same 16 synthetic bytes in lease-ID and listen-history wire encodings.
const playId = 'QMMlwCzuSJ2otvvpdBVWAQ';
const start: ListenEvent = { playback_id: '40c325c0-2cee-489d-a8b6-fbe974155601', track_id: 'catalogue-42', event: 'start' };
const complete: ListenEvent = { ...start, event: 'complete' };
const ack = () => Response.json({ ok: true });

function fixture(fetchFn = vi.fn<typeof fetch>(), canRecord = () => true) {
	const account = new AbortController();
	const owner: AccountSession = {
		generation: 'synthetic-account', signal: account.signal, isCurrent: () => !account.signal.aborted,
		run: async (work) => !account.signal.aborted ? work() : undefined
	};
	let source: BrowserPlaybackSource | undefined;
	const grant = createBrowserPlaybackGrant(owner, binding, async ({ sessionBinding, playbackSessionId, trackId }) => ({
		data: { sessionBinding, playbackSessionId, trackId, playId, durationMs: 60_000 }
	}));
	const sender = createListenEventSender({ owner, canRecord, grantForStart: () => source, fetchFn });
	onTestFinished(() => { sender.dispose(); grant.dispose(); });
	return {
		sender, account, grant, fetchFn,
		openGrant: () => grant.open({ trackId: start.track_id }, account.signal, () => true, (issued) => { source = issued; }),
		bodies: () => fetchFn.mock.calls.filter(([path]) => path === '/v1/plays/events')
			.map(([, init]) => JSON.parse(String(init?.body)))
	};
}

function contextFetch() {
	return vi.fn<typeof fetch>().mockResolvedValueOnce(Response.json({ context }));
}

afterEach(() => vi.useRealTimers());

it('sends only for the matching grant and orders the terminal after its accepted start', async () => {
	const pending = Promise.withResolvers<Response>();
	const fetchFn = contextFetch().mockImplementationOnce(() => pending.promise).mockImplementation(async () => ack());
	const f = fixture(fetchFn);
	await f.sender.ready;
	f.sender.send(start);
	await Promise.resolve();
	expect(f.bodies()).toEqual([]);
	expect(await f.openGrant()).toBe(true);
	f.sender.send({ ...start, track_id: 'different' });
	f.sender.send({ ...start, playback_id: '11111111-1111-4111-8111-111111111111' });
	f.sender.send(start);
	f.sender.send(start);
	f.sender.send(complete);
	f.sender.send({ ...start, event: 'skip' });
	await vi.waitFor(() => expect(f.bodies()).toEqual([{ ...start, context }]));
	pending.resolve(ack());
	await vi.waitFor(() => expect(f.bodies()).toEqual([{ ...start, context }, { ...complete, context }]));
	expect(fetchFn.mock.calls.map(([path]) => path)).toEqual([
		'/v1/plays/events/context', '/v1/plays/events', '/v1/plays/events'
	]);
	for (const [, init] of fetchFn.mock.calls) {
		expect(init).toMatchObject({ method: 'POST', mode: 'same-origin', credentials: 'same-origin', cache: 'no-store', redirect: 'error' });
		expect(new Headers(init?.headers).has('authorization')).toBe(false);
	}
});

it.each(['malformed', 'refused', 'pending', 'opted out', 'retired grant'])(
	'drops events for %s context or authority', async (scenario) => {
		const pending = Promise.withResolvers<Response>();
		const fetchFn = vi.fn<typeof fetch>().mockImplementation(() => pending.promise);
		const f = fixture(fetchFn, () => scenario !== 'opted out');
		expect(await f.openGrant()).toBe(true);
		if (scenario === 'retired grant') f.grant.dispose();
		if (scenario !== 'pending') {
			pending.resolve(scenario === 'malformed' ? Response.json({ context: `${context}\n` }) :
				scenario === 'refused' ? new Response(null, { status: 404 }) : Response.json({ context }));
			await f.sender.ready;
		}
		f.sender.send(start);
		if (scenario === 'pending') {
			pending.resolve(Response.json({ context }));
			await f.sender.ready;
		}
		f.sender.send(complete);
		await f.sender.close();
		expect(fetchFn).toHaveBeenCalledTimes(scenario === 'opted out' ? 0 : 1);
	});

it.each([401, 409, 422, 200])('drops the terminal after an unaccepted start (%s)', async (status) => {
	vi.useFakeTimers();
	const fetchFn = contextFetch().mockResolvedValueOnce(Response.json({ ok: false }, { status }));
	const f = fixture(fetchFn);
	await f.sender.ready;
	await f.openGrant();
	f.sender.send(start);
	f.sender.send(complete);
	await vi.advanceTimersByTimeAsync(2500);
	if (status === 401) {
		f.sender.send(start);
		await vi.advanceTimersByTimeAsync(2500);
	}
	expect(f.bodies()).toEqual([{ ...start, context }]);
});

it('bounds transient retries and honors a capped Retry-After', async () => {
	vi.useFakeTimers();
	const fetchFn = contextFetch().mockRejectedValueOnce(new TypeError('offline'))
		.mockResolvedValueOnce(new Response(null, { status: 429, headers: { 'retry-after': '10000' } }))
		.mockResolvedValueOnce(new Response(null, { status: 503 }));
	const f = fixture(fetchFn);
	await f.sender.ready;
	await f.openGrant();
	f.sender.send(start);
	f.sender.send(complete);
	await vi.advanceTimersByTimeAsync(60_499);
	expect(f.bodies()).toEqual(Array(2).fill({ ...start, context }));
	await vi.advanceTimersByTimeAsync(1);
	expect(f.bodies()).toEqual(Array(3).fill({ ...start, context }));
	await vi.advanceTimersByTimeAsync(20_000);
	expect(f.bodies()).toEqual(Array(3).fill({ ...start, context }));
});

it('replays a start with an interrupted acknowledgement before sending its terminal', async () => {
	vi.useFakeTimers();
	const interrupted = new Response(new ReadableStream({
		start: (controller) => controller.enqueue(new TextEncoder().encode('{"ok":')),
		pull: (controller) => controller.error(new TypeError('connection interrupted'))
	}));
	const fetchFn = contextFetch().mockResolvedValueOnce(interrupted).mockImplementation(async () => ack());
	const f = fixture(fetchFn);
	await f.sender.ready;
	await f.openGrant();
	f.sender.send(start);
	f.sender.send(complete);
	await vi.advanceTimersByTimeAsync(499);
	expect(f.bodies()).toEqual([{ ...start, context }]);
	await vi.advanceTimersByTimeAsync(1);
	expect(f.bodies()).toEqual([{ ...start, context }, { ...start, context }, { ...complete, context }]);
});

it('retries when the request deadline aborts a stalled acknowledgement', async () => {
	vi.useFakeTimers();
	const deadline = new AbortController();
	const fetchFn = contextFetch().mockImplementationOnce(async (_path, init) => new Response(new ReadableStream({
		start(controller) {
			controller.enqueue(new TextEncoder().encode('{"ok":'));
			init?.signal?.addEventListener('abort', () => controller.error(init.signal?.reason), { once: true });
		}
	}))).mockImplementation(async () => ack());
	const f = fixture(fetchFn);
	await f.sender.ready;
	await f.openGrant();
	const timeout = vi.spyOn(AbortSignal, 'timeout').mockReturnValueOnce(deadline.signal);
	onTestFinished(() => { timeout.mockRestore(); });
	f.sender.send(start);
	f.sender.send(complete);
	await vi.advanceTimersByTimeAsync(0);
	expect(timeout).toHaveBeenCalledWith(10_000);
	deadline.abort(new DOMException('fixture deadline', 'TimeoutError'));
	await vi.advanceTimersByTimeAsync(499);
	expect(f.bodies()).toEqual([{ ...start, context }]);
	await vi.advanceTimersByTimeAsync(1);
	expect(f.account.signal.aborted).toBe(false);
	expect(f.bodies()).toEqual([{ ...start, context }, { ...start, context }, { ...complete, context }]);
});

it('aborts in-flight account work and disregards a late acknowledgement', async () => {
	const pending = Promise.withResolvers<Response>();
	const fetchFn = contextFetch().mockImplementationOnce(() => pending.promise);
	const f = fixture(fetchFn);
	await f.sender.ready;
	await f.openGrant();
	f.sender.send(start);
	f.sender.send(complete);
	await vi.waitFor(() => expect(fetchFn).toHaveBeenCalledTimes(2));
	f.account.abort();
	expect(fetchFn.mock.calls[1][1]?.signal?.aborted).toBe(true);
	pending.resolve(ack());
	await f.sender.close();
	expect(f.bodies()).toEqual([{ ...start, context }]);
});

it('drains only accepted terminals and cancels their pending retry on close', async () => {
	vi.useFakeTimers();
	const fetchFn = contextFetch().mockImplementationOnce(async () => ack())
		.mockResolvedValueOnce(new Response(null, { status: 503 }));
	const f = fixture(fetchFn);
	await f.sender.ready;
	await f.openGrant();
	f.sender.send(start);
	await vi.advanceTimersByTimeAsync(0);
	f.sender.send(complete);
	await vi.advanceTimersByTimeAsync(0);
	await f.sender.close();
	await vi.advanceTimersByTimeAsync(2500);
	expect(f.bodies()).toEqual([{ ...start, context }, { ...complete, context }]);
});

it('cancels a pending start on close and drops its queued terminal after a late acknowledgement', async () => {
	vi.useFakeTimers();
	const pendingStart = Promise.withResolvers<Response>();
	const fetchFn = contextFetch().mockImplementationOnce(() => pendingStart.promise)
		.mockImplementation(async () => ack());
	const f = fixture(fetchFn);
	await f.sender.ready;
	expect(await f.openGrant()).toBe(true);
	f.sender.send(start);
	f.sender.send(complete);
	await vi.advanceTimersByTimeAsync(0);
	expect(f.bodies()).toEqual([{ ...start, context }]);
	const signal = fetchFn.mock.calls[1][1]?.signal;
	expect(signal?.aborted).toBe(false);

	const closed = vi.fn();
	const closing = f.sender.close().then(closed);
	await vi.advanceTimersByTimeAsync(0);
	expect(closed).toHaveBeenCalledOnce();
	expect(signal?.aborted).toBe(true);
	// A response delivered after cancellation cannot release the queued terminal.
	pendingStart.resolve(ack());
	await closing;
	await vi.advanceTimersByTimeAsync(2_500);
	expect(f.bodies()).toEqual([{ ...start, context }]);
});

it('waits on a ready terminal during close and aborts the drain when the account retires', async () => {
	vi.useFakeTimers();
	const terminal = Promise.withResolvers<Response>();
	const fetchFn = contextFetch().mockImplementationOnce(async () => ack()).mockImplementationOnce(() => terminal.promise);
	const f = fixture(fetchFn);
	await f.sender.ready;
	await f.openGrant();
	f.sender.send(start);
	await vi.advanceTimersByTimeAsync(0);
	f.sender.send(complete);
	const drained = vi.fn();
	const closing = f.sender.close().then(drained);
	await vi.advanceTimersByTimeAsync(0);
	expect(f.bodies()).toEqual([{ ...start, context }, { ...complete, context }]);
	expect(drained).not.toHaveBeenCalled();
	f.account.abort();
	expect(fetchFn.mock.calls[2][1]?.signal?.aborted).toBe(true);
	terminal.resolve(ack());
	await closing;
	expect(drained).toHaveBeenCalledOnce();
});
