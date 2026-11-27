import { expect, it, vi } from 'vitest';
import { JsonBodyTooLargeError, MAX_JSON_BODY_BYTES, readCappedJson } from './cappedJson.js';
import { API_REQUEST_TIMEOUT_MS, ApiClient, ApiError } from './client.js';
import { parseErrorBody } from './errors.js';
import { cspSources } from '../security/headers.js';
import { bindVerifiedLocalLyrics, createLyricsResolver, type ExactLyricsProvider } from '../server/lyrics.js';

it('rejects oversized JSON without waiting for upstream cancellation to settle', async () => {
	const cancel = vi.fn(() => new Promise<void>(() => {}));
	const response = new Response(new ReadableStream<Uint8Array>({
		start: (controller) => controller.enqueue(new Uint8Array(MAX_JSON_BODY_BYTES + 1)),
		cancel
	}));

	await expect(readCappedJson(response)).rejects.toBeInstanceOf(JsonBodyTooLargeError);
	expect(cancel).toHaveBeenCalledOnce();
	expect(response.body?.locked).toBe(false);
}, 500);

it('rejects malformed UTF-8 as a typed failure even inside an otherwise valid JSON string', async () => {
	const fetchFn = vi.fn(async () => new Response(Uint8Array.of(0x22, 0xc0, 0x22)));
	const client = new ApiClient({ baseUrl: '', fetchFn });
	await expect(client.get('/health')).rejects.toMatchObject({
		name: 'ApiError', status: 200, code: 'request_failed', retryable: false
	});
});

it('uses a typed status fallback for excessively nested paywall JSON', async () => {
	const body = '{"usage":'.repeat(15_000) + '{}' + '}'.repeat(15_000);
	const client = new ApiClient({ baseUrl: '', fetchFn: async () => new Response(body, { status: 402 }) });
	const failure = await client.get('/health').catch((error: unknown) => error);
	expect(failure).toBeInstanceOf(ApiError);
	expect(failure).toMatchObject({
		status: 402, message: 'request failed with status 402', paywall: { kind: 'terminal' }
	});
});

it('does not count braces or escaped quotes in JSON strings toward the nesting limit', async () => {
	const detail = '{["\\'.repeat(150);
	await expect(readCappedJson(Response.json({ detail }))).resolves.toEqual({ detail });
});

it.each(['//other.example/health', '/\\other.example/health', '/\t/other.example/health',
	'/prefix/%2e%2e/v1/session/logout', 'other.example/health', '/health#fragment'])(
	'refuses ambiguous request targets before fetching (%j)', async (path) => {
		const fetchFn = vi.fn(async () => Response.json({ ok: true }));
		const client = new ApiClient({ baseUrl: '', fetchFn });
		await expect(client.get(path)).rejects.toThrow();
		expect(fetchFn).not.toHaveBeenCalled();
	}
);

it('preserves a configured API path and encoded query while refusing redirects', async () => {
	const fetchFn = vi.fn(async () => Response.json({ ok: true }));
	const client = new ApiClient({ baseUrl: 'https://api.example:443/prefix/', fetchFn });
	await expect(client.post('/v1/auth/magic/finish?next=%2Fhome', {})).resolves.toEqual({ ok: true });
	expect(fetchFn).toHaveBeenCalledWith('https://api.example/prefix/v1/auth/magic/finish?next=%2Fhome',
		expect.objectContaining({ redirect: 'error' }));
});

it('requires a session binding for protected routes under a configured API prefix', async () => {
	const fetchFn = vi.fn(async () => Response.json({ ok: true }));
	const client = new ApiClient({ baseUrl: 'https://api.example/prefix', fetchFn });
	await expect(client.get('/v1/me/taste/session?cursor=next')).rejects.toThrow('session binding');
	expect(fetchFn).not.toHaveBeenCalled();
});

it('times out even when the transport never settles after abort', async () => {
	vi.useFakeTimers();
	try {
		const client = new ApiClient({ baseUrl: '', fetchFn: () => new Promise(() => {}) });
		const failure = vi.fn();
		void client.get('/health').catch(failure);
		await vi.advanceTimersByTimeAsync(API_REQUEST_TIMEOUT_MS);
		expect(failure).toHaveBeenCalledWith(expect.objectContaining({ code: 'request_timeout' }));
	} finally {
		vi.useRealTimers();
	}
});

it('cancels a stalled body reader when the request deadline expires', async () => {
	vi.useFakeTimers();
	try {
		const cancel = vi.fn();
		const body = new ReadableStream<Uint8Array>({ cancel });
		const fetchFn = vi.fn(async () => new Response(body));
		const client = new ApiClient({ baseUrl: '', fetchFn });
		const failure = client.get('/health').catch((error: unknown) => error);
		await vi.advanceTimersByTimeAsync(API_REQUEST_TIMEOUT_MS);
		expect(await failure).toMatchObject({ code: 'request_timeout' });
		expect(cancel).toHaveBeenCalledOnce();
		expect(body.locked).toBe(false);
	} finally {
		vi.useRealTimers();
	}
});

it('uses legacy provider signals only on a 401 without a string code', () => {
	expect(parseErrorBody({ code: 'reauth_required', error: 'indexer_auth_expired' }, 401).code)
		.toBe('reauth_required');
	expect(parseErrorBody({ code: 'provider_auth_expired' }, 503).code).toBe('request_failed');
	expect(parseErrorBody({ error: 'indexer_auth_expired' }, 503).code).toBe('service_unavailable');
	expect(parseErrorBody({ error: 'provider_auth_expired' }, 401).code).toBe('provider_auth_expired');
});

it.each(['https://api.example;invalid', 'https://api.example,invalid', 'https://@api.example',
	'https:///api.example', 'https://api.example?', 'https://api.example#', 'https://api.ex\tample'])(
	'rejects an invalid API base in both CSP and runtime configuration (%j)', (baseUrl) => {
		expect(() => cspSources(baseUrl)).toThrow();
		expect(() => new ApiClient({ baseUrl })).toThrow();
	}
);

const track = { trackName: 'Track', artistName: 'Artist', albumName: 'Album', durationSeconds: 120 };
const lrc = '[00:01]Fixture cue';

it('fails closed for malformed local lyrics metadata', async () => {
	const resolve = createLyricsResolver();
	await expect(resolve({ mediaPath: 7 as unknown as string, sidecar: { path: '7.lrc', lrc } }))
		.resolves.toBeNull();
	const identity = { libraryScope: Symbol(), trackId: 'x', occurrenceKey: 'y', sourceRevision: 'z' };
	expect(bindVerifiedLocalLyrics(identity, identity, {
		identity, embedded: lrc, instrumental: 0 as unknown as boolean
	})).toBeNull();
});

it('does not negative-cache an instrumental provider reply with malformed lyrics metadata', async () => {
	const provider = vi.fn<ExactLyricsProvider>()
		.mockResolvedValueOnce({ kind: 'match', track, instrumental: true, syncedLyrics: 7 as unknown as string })
		.mockResolvedValueOnce({ kind: 'match', track, instrumental: false, syncedLyrics: lrc });
	const resolve = createLyricsResolver(provider);
	await expect(resolve({}, track)).resolves.toBeNull();
	await expect(resolve({}, track)).resolves.toMatchObject({ source: 'lrclib' });
	expect(provider).toHaveBeenCalledTimes(2);
});
