import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { MAX_JSON_BODY_BYTES } from '#lib/api/cappedJson.js';
import { captureAuthFence } from '#lib/auth-fence.js';
import { createRecentListensReader } from './recent-listens.js';

vi.mock('#lib/auth-fence.js', () => ({ captureAuthFence: vi.fn() }));

const baseUrl = 'https://api.example.test';
const playbackId = '10000000-0000-0000-0000-000000000000';
const fixtureId = (i: number) => `${playbackId.slice(0, -2)}${i.toString(16).padStart(2, '0')}`;
const row = { playback_id: playbackId, track_id: 'example-track', event: 'skip', recorded_at: 9 };
const page = (events: unknown[], cursor: string | null = null) =>
	Response.json({ events, next_cursor: cursor, retention: 'account_lifetime' });
const fetchPage = vi.fn<typeof fetch>();
const publish = vi.fn();
let current: boolean;

beforeEach(() => {
	vi.resetAllMocks();
	current = true;
	vi.mocked(captureAuthFence).mockResolvedValue({
		generation: 'test',
		signal: new AbortController().signal,
		isCurrent: () => current,
		run: async (work) => current ? work() : undefined
	});
	vi.stubGlobal('fetch', fetchPage);
});
afterEach(() => {
	vi.unstubAllGlobals();
	vi.restoreAllMocks();
	vi.useRealTimers();
});

it('posts one cookie-only first-page request and preserves server order, phases, and timestamps', async () => {
	fetchPage.mockResolvedValue(page([
		row,
		{ ...row, event: 'start', recorded_at: 10 },
		...Array.from({ length: 98 }, (_, i) => ({ ...row, playback_id: fixtureId(i + 1) }))
	], 'opaque-cursor'));
	await createRecentListensReader(`${baseUrl}/`, publish).load();

	expect(fetchPage).toHaveBeenCalledExactlyOnceWith(`${baseUrl}/v1/plays/export`, {
		method: 'POST', credentials: 'include', cache: 'no-store', redirect: 'error', referrerPolicy: 'origin',
		headers: { accept: 'application/json', 'content-type': 'application/json' }, body: '{}',
		signal: expect.any(AbortSignal)
	});
	expect(publish).toHaveBeenNthCalledWith(1, { kind: 'loading' });
	expect(publish).toHaveBeenLastCalledWith({ kind: 'ready', events: [
		{ key: `${playbackId}:1`, track: 'example-track', event: 'skip', recordedAt: new Date(9000) },
		{ key: `${playbackId}:0`, track: 'example-track', event: 'start', recordedAt: new Date(10000) },
		...[1, 2, 3].map((i) => expect.objectContaining({ key: `${fixtureId(i)}:1` }))
	] });
});

it.each([
	['empty', () => page([])],
	['sign-in', () => Response.json({ code: 'unauthenticated' }, { status: 401 })],
	['refused', () => Response.json({ code: 'forbidden' }, { status: 403 })],
	['error', () => new Response(null, { status: 503 })],
	['error', () => page([{ ...row, event: 'unknown' }])],
	['error', () => page([row, { ...row, event: 'complete' }])],
	['error', () => page([], 'opaque-cursor')],
	['error', () => page([row], 'opaque-cursor')],
	['error', () => page(Array.from({ length: 101 }, (_, i) => ({ ...row, playback_id: fixtureId(i) })))]
] as const)('shows %s without listening rows', async (kind, response) => {
	fetchPage.mockResolvedValue(response());
	await createRecentListensReader(baseUrl, publish).load();
	expect(publish).toHaveBeenLastCalledWith({ kind });
});

it('cancels a streamed export over the byte cap without publishing rows', async () => {
	const bytes = new TextEncoder().encode(await page([row]).text() + ' '.repeat(MAX_JSON_BODY_BYTES));
	const cancel = vi.fn();
	fetchPage.mockResolvedValue(new Response(new ReadableStream({
		start: (controller) => controller.enqueue(bytes),
		pull: (controller) => controller.close(),
		cancel
	}, { highWaterMark: 0 })));
	await createRecentListensReader(baseUrl, publish).load();
	expect(cancel).toHaveBeenCalledOnce();
	expect(publish.mock.calls).toEqual([[{ kind: 'loading' }], [{ kind: 'error' }]]);
});

it('keeps the request deadline active while reading a stalled 200 body', async () => {
	vi.useFakeTimers();
	vi.spyOn(AbortSignal, 'timeout').mockImplementation((ms) => {
		const deadline = new AbortController();
		setTimeout(() => deadline.abort(), ms);
		return deadline.signal;
	});
	fetchPage.mockImplementation(async (_url, init) => new Response(new ReadableStream({
		start: (controller) => init?.signal?.addEventListener('abort', () => controller.error(init.signal?.reason))
	})));
	const load = createRecentListensReader(baseUrl, publish).load();
	await vi.advanceTimersByTimeAsync(9_999);
	expect(publish.mock.calls).toEqual([[{ kind: 'loading' }]]);
	await vi.advanceTimersByTimeAsync(1);
	expect(fetchPage.mock.calls[0]?.[1]?.signal?.aborted).toBe(true);
	await load;
	expect(publish.mock.calls).toEqual([[{ kind: 'loading' }], [{ kind: 'error' }]]);
});

it('drops a late prior-session response even before an invalidation event is delivered', async () => {
	const pending = Promise.withResolvers<Response>();
	const started = Promise.withResolvers<void>();
	fetchPage.mockImplementationOnce(() => {
		started.resolve();
		return pending.promise;
	});
	const reader = createRecentListensReader(baseUrl, publish);
	const oldLoad = reader.load();
	await started.promise;
	current = false;
	pending.resolve(page([row]));
	await oldLoad;
	expect(publish.mock.calls).toEqual([[{ kind: 'loading' }], [{ kind: 'stale' }]]);

	current = true;
	fetchPage.mockResolvedValueOnce(page([]));
	await reader.load();
	expect(publish).toHaveBeenLastCalledWith({ kind: 'empty' });
});

it('aborts an invalidated request and does not replace a newer result if transport ignores abort', async () => {
	const pending = Promise.withResolvers<Response>();
	const started = Promise.withResolvers<AbortSignal | null | undefined>();
	fetchPage.mockImplementationOnce((_url, init) => {
		started.resolve(init?.signal);
		return pending.promise;
	});
	const reader = createRecentListensReader(baseUrl, publish);
	const oldLoad = reader.load();
	const signal = await started.promise;
	reader.invalidate();
	expect(signal?.aborted).toBe(true);
	expect(publish).toHaveBeenLastCalledWith({ kind: 'stale' });

	fetchPage.mockResolvedValueOnce(page([]));
	await reader.load();
	pending.resolve(page([row]));
	await oldLoad;
	expect(publish.mock.calls).toEqual([
		[{ kind: 'loading' }], [{ kind: 'stale' }], [{ kind: 'loading' }], [{ kind: 'empty' }]
	]);
});

it('does not request history when a browser auth fence cannot be captured', async () => {
	vi.mocked(captureAuthFence).mockResolvedValueOnce(undefined);
	await createRecentListensReader(baseUrl, publish).load();
	expect(fetchPage).not.toHaveBeenCalled();
	expect(publish).toHaveBeenLastCalledWith({ kind: 'stale' });
});
