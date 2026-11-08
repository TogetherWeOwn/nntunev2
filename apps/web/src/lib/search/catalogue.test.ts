import { afterEach, describe, expect, it, vi } from 'vitest';

import { CatalogueSearchError, searchCatalogue } from './catalogue.js';

const BASE = 'https://api.example.test';
const A1 = '11111111-1111-4111-8111-111111111111';
const A2 = '22222222-2222-4222-8222-222222222222';
const R1 = '33333333-3333-4333-8333-333333333333';
const T1 = '44444444-4444-4444-8444-444444444444';

function json(body: unknown, init: ResponseInit = {}): Response {
	return new Response(JSON.stringify(body), {
		status: 200,
		headers: { 'content-type': 'application/json' },
		...init
	});
}

function envelope(type: string, results: unknown[]) {
	return { q: 'x', type, limit: 12, offset: 0, results: results.map((result) =>
		typeof result === 'object' && result !== null && !Array.isArray(result)
			? { availability: 'available', destination: null, occurrences: [], ...result } : result
	) };
}

function okFetch(): typeof fetch {
	return vi.fn(async (input: RequestInfo | URL) => {
		const type = new URL(String(input)).searchParams.get('type')!;
		if (type === 'artist') return json(envelope(type, [{ mbid: A1, name: 'Alpha', score: 90 }]));
		if (type === 'release')
			return json(
				envelope(type, [
					{ mbid: R1, title: 'Rel', score: 95, artist_mbid: A1, artist_name: 'Alpha' }
				])
			);
		return json(envelope(type, [{ mbid: T1, title: 'Rec', score: 80, artist_mbid: null, artist_name: null,
			occurrences: [{ id: T1, occurrence_key: 'job:1' }] }]));
	}) as unknown as typeof fetch;
}

afterEach(() => {
	vi.useRealTimers();
});

describe('searchCatalogue request', () => {
	it('issues three parallel credentialed JSON requests with encoded query', async () => {
		const fetchFn = okFetch();
		const hits = await searchCatalogue(
			`${BASE}/`,
			'  AC/DC & Co? é ',
			new AbortController().signal,
			fetchFn
		);
		const calls = (fetchFn as unknown as ReturnType<typeof vi.fn>).mock.calls;
		expect(calls).toHaveLength(3);
		const types: string[] = [];
		for (const [url, init] of calls) {
			const u = String(url);
			expect(u.startsWith(`${BASE}/v1/mb/search?q=AC%2FDC%20%26%20Co%3F%20%C3%A9&type=`)).toBe(true);
			expect(u.endsWith('&limit=12&offset=0')).toBe(true);
			types.push(new URL(u).searchParams.get('type')!);
			expect(init.credentials).toBe('include');
			expect(init.method).toBe('GET');
			expect(init.headers.accept).toBe('application/json');
		}
		expect(types.sort()).toEqual(['artist', 'recording', 'release']);
		expect(hits.map((h) => [h.kind, h.mbid])).toEqual([
			['release', R1],
			['artist', A1],
			['recording', T1]
		]);
		expect(hits[0]).toMatchObject({ artistMbid: A1, artistName: 'Alpha' });
		expect(hits[2].artistMbid).toBeUndefined();
	});
});

describe('searchCatalogue query validation', () => {
	it.each([
		['empty', ''],
		['whitespace', '   '],
		['one char', ' a '],
		['one astral char', '\u{1F3B5}'],
		['too long', 'a'.repeat(101)],
		['newline', 'ab\ncd'],
		['nul', 'ab\u0000cd'],
		['del/c1', 'ab\u0085cd'],
		['line separator', 'ab\u2028cd']
	])('returns [] without fetching for %s', async (_n, q) => {
		const fetchFn = vi.fn();
		await expect(
			searchCatalogue(BASE, q, new AbortController().signal, fetchFn as unknown as typeof fetch)
		).resolves.toEqual([]);
		expect(fetchFn).not.toHaveBeenCalled();
	});

	it('counts Unicode code points, not UTF-16 units', async () => {
		const fetchFn = okFetch();
		await searchCatalogue(BASE, '\u{1F3B5}'.repeat(100), new AbortController().signal, fetchFn);
		expect(fetchFn).toHaveBeenCalledTimes(3);
		const none = vi.fn();
		await searchCatalogue(BASE, '\u{1F3B5}'.repeat(101), new AbortController().signal, none as unknown as typeof fetch);
		expect(none).not.toHaveBeenCalled();
	});
});

describe('searchCatalogue response validation', () => {
	it.each([
		['legacy raw metadata', {}],
		['unknown readiness', { availability: 'unknown', destination: null, occurrences: [] }],
		['unavailable default response', { availability: 'unavailable', destination: null, occurrences: [] }],
		['foreign destination', { availability: 'available', destination: 'https://other.test/', occurrences: [] }],
		['unvalidated entity destination', { availability: 'available', destination: `/artist/${A2}`, occurrences: [] }],
		['missing occurrence identity', { availability: 'available', destination: null, occurrences: [{ id: 'one' }] }]
	])('fails to library-only fallback for %s', async (_name, fields) => {
		const fetchFn = vi.fn(async (input: RequestInfo | URL) => {
			const type = new URL(String(input)).searchParams.get('type')!;
			return json({ type, results: type === 'artist' ? [{ mbid: A1, name: 'Alpha', score: 1, ...fields }] : [] });
		}) as unknown as typeof fetch;
		await expect(searchCatalogue(BASE, 'alpha', new AbortController().signal, fetchFn)).rejects.toMatchObject({ code: 'failed' });
	});

	it('requests unavailable rows only with an explicit opt-in and retains their non-actionable shape', async () => {
		const fetchFn = vi.fn(async (input: RequestInfo | URL) => {
			const url = new URL(String(input));
			const type = url.searchParams.get('type')!;
			expect(url.searchParams.get('include_unavailable')).toBe('true');
			return json(envelope(type, [{ mbid: A1, [type === 'artist' ? 'name' : 'title']: 'Unavailable',
				score: 1, availability: 'unavailable', destination: null, occurrences: [] }]));
		}) as unknown as typeof fetch;
		const hits = await searchCatalogue(BASE, 'alpha', new AbortController().signal, fetchFn, { includeUnavailable: true });
		expect(hits).toHaveLength(3);
		expect(hits.every((hit) => hit.availability === 'unavailable' && hit.destination === null && hit.occurrences.length === 0)).toBe(true);
	});

	it.each([
		['destination', { availability: 'unavailable', destination: `/artist/${A1}`, occurrences: [] }],
		['playable occurrence', { availability: 'unavailable', destination: null, occurrences: [{ id: 'one', occurrence_key: 'job:1' }] }],
		['unknown readiness', { availability: 'unknown', destination: null, occurrences: [] }],
		['missing readiness', { destination: null, occurrences: [] }]
	])('rejects %s even when unavailable rows are requested', async (_name, fields) => {
		const fetchFn = vi.fn(async (input: RequestInfo | URL) => {
			const type = new URL(String(input)).searchParams.get('type')!;
			return json({ type, results: type === 'artist' ? [{ mbid: A1, name: 'Alpha', score: 1, ...fields }] : [] });
		}) as unknown as typeof fetch;
		await expect(searchCatalogue(BASE, 'alpha', new AbortController().signal, fetchFn, { includeUnavailable: true }))
			.rejects.toMatchObject({ code: 'failed' });
	});

	it.each([401, 403, 404])('does not treat an include-unavailable denial (%s) as successful admin access', async (status) => {
		const fetchFn = vi.fn(async () => new Response('private denial detail', { status })) as unknown as typeof fetch;
		await expect(searchCatalogue(BASE, 'alpha', new AbortController().signal, fetchFn, { includeUnavailable: true }))
			.rejects.toMatchObject({ code: 'failed', message: 'Catalogue search failed.' });
	});

	it('accepts only a validated own-origin entity destination', async () => {
		const fetchFn = vi.fn(async (input: RequestInfo | URL) => {
			const type = new URL(String(input)).searchParams.get('type')!;
			return json(envelope(type, type === 'artist' ? [{ mbid: A1, name: 'Alpha', score: 1, destination: `/artist/${A1}` }] : []));
		}) as unknown as typeof fetch;
		expect(await searchCatalogue(BASE, 'alpha', new AbortController().signal, fetchFn)).toMatchObject([{ destination: `/artist/${A1}` }]);
	});
	function withArtist(results: unknown[]): typeof fetch {
		return vi.fn(async (input: RequestInfo | URL) => {
			const type = new URL(String(input)).searchParams.get('type')!;
			return json(envelope(type, type === 'artist' ? results : []));
		}) as unknown as typeof fetch;
	}

	it('drops malformed hits and non-UUID ids', async () => {
		const hits = await searchCatalogue(
			BASE,
			'alpha',
			new AbortController().signal,
			withArtist([
				{ mbid: 'not-a-uuid', name: 'Bad', score: 1 },
				{ mbid: A2, name: '', score: 1 },
				{ mbid: A2, name: 'NoScore' },
				{ mbid: A2, name: 'NaN', score: 'x' },
				'junk',
				null,
				{ mbid: A1, name: 'Good', score: 5 }
			])
		);
		expect(hits).toEqual([{ mbid: A1, label: 'Good', score: 5, kind: 'artist', availability: 'available', destination: null, occurrences: [] }]);
	});

	it('drops an invalid artist link on a release but keeps the hit', async () => {
		const fetchFn = vi.fn(async (input: RequestInfo | URL) => {
			const type = new URL(String(input)).searchParams.get('type')!;
			return json(
				envelope(
					type,
					type === 'release'
						? [{ mbid: R1, title: 'Rel', score: 1, artist_mbid: 'javascript:1', artist_name: 'X' }]
						: []
				)
			);
		}) as unknown as typeof fetch;
		const hits = await searchCatalogue(BASE, 'rel', new AbortController().signal, fetchFn);
		expect(hits).toHaveLength(1);
		expect(hits[0].artistMbid).toBeUndefined();
	});

	it('dedupes by kind and mbid keeping the best score, ordering deterministically', async () => {
		const hits = await searchCatalogue(
			BASE,
			'alpha',
			new AbortController().signal,
			withArtist([
				{ mbid: A2, name: 'B', score: 50 },
				{ mbid: A1, name: 'A', score: 50 },
				{ mbid: A1.toUpperCase(), name: 'A again', score: 70 }
			])
		);
		expect(hits.map((h) => [h.mbid, h.score])).toEqual([
			[A1, 70],
			[A2, 50]
		]);
	});

	it.each([
		['non-object', []],
		['missing results', { type: 'artist' }],
		['results not array', { type: 'artist', results: {} }],
		['wrong type', { type: 'release', results: [] }]
	])('rejects a malformed envelope (%s)', async (_n, body) => {
		const fetchFn = vi.fn(async () => json(body)) as unknown as typeof fetch;
		await expect(
			searchCatalogue(BASE, 'alpha', new AbortController().signal, fetchFn)
		).rejects.toMatchObject({ code: 'failed' });
	});

	it('rejects invalid JSON and over-cap bodies with a generic error', async () => {
		const bad = vi.fn(async () => new Response('{nope')) as unknown as typeof fetch;
		await expect(
			searchCatalogue(BASE, 'alpha', new AbortController().signal, bad)
		).rejects.toBeInstanceOf(CatalogueSearchError);

		const big = vi.fn(
			async () => new Response(`{"pad":"${'x'.repeat(1_048_576)}"}`)
		) as unknown as typeof fetch;
		await expect(
			searchCatalogue(BASE, 'alpha', new AbortController().signal, big)
		).rejects.toMatchObject({ code: 'failed', message: 'Catalogue search failed.' });
	});
});

describe('searchCatalogue failures', () => {
	it('fails everything when one request returns an error status, without leaking remote copy', async () => {
		const fetchFn = vi.fn(async (input: RequestInfo | URL) => {
			const type = new URL(String(input)).searchParams.get('type')!;
			if (type === 'release') return new Response('SECRET-REMOTE-TEXT', { status: 503 });
			return json(envelope(type, []));
		}) as unknown as typeof fetch;
		const err = await searchCatalogue(BASE, 'alpha', new AbortController().signal, fetchFn).catch(
			(e) => e
		);
		expect(err).toBeInstanceOf(CatalogueSearchError);
		expect(err.code).toBe('failed');
		expect(err.message).not.toContain('SECRET');
	});

	it('fails everything when one request rejects', async () => {
		const fetchFn = vi.fn(async (input: RequestInfo | URL) => {
			const type = new URL(String(input)).searchParams.get('type')!;
			if (type === 'recording') throw new TypeError('network down: secret-host');
			return json(envelope(type, []));
		}) as unknown as typeof fetch;
		const err = await searchCatalogue(BASE, 'alpha', new AbortController().signal, fetchFn).catch(
			(e) => e
		);
		expect(err.code).toBe('failed');
		expect(err.message).not.toContain('secret');
	});

	it('times out even when the transport ignores the signal, and never publishes a late result', async () => {
		vi.useFakeTimers();
		let release!: () => void;
		const gate = new Promise<void>((r) => (release = r));
		const fetchFn = vi.fn(async (input: RequestInfo | URL) => {
			await gate;
			const type = new URL(String(input)).searchParams.get('type')!;
			return json(envelope(type, [{ mbid: A1, name: 'Late', score: 1 }]));
		}) as unknown as typeof fetch;
		const outcome = searchCatalogue(BASE, 'alpha', new AbortController().signal, fetchFn).then(
			(v) => ({ v }),
			(e) => ({ e })
		);
		await vi.advanceTimersByTimeAsync(8000);
		const result = await outcome;
		expect('e' in result && result.e.code).toBe('timeout');
		release();
		await vi.advanceTimersByTimeAsync(0);
		expect('v' in result).toBe(false);
	});

	it('timeout covers a stalled body read', async () => {
		vi.useFakeTimers();
		const fetchFn = vi.fn(async () => {
			const stream = new ReadableStream<Uint8Array>({ start() {} });
			return new Response(stream, { status: 200 });
		}) as unknown as typeof fetch;
		const outcome = searchCatalogue(BASE, 'alpha', new AbortController().signal, fetchFn).catch(
			(e) => e
		);
		await vi.advanceTimersByTimeAsync(8000);
		await expect(outcome).resolves.toMatchObject({ code: 'timeout' });
	});

	it('rejects immediately when the caller aborts, even if the transport ignores the signal', async () => {
		const ctl = new AbortController();
		const fetchFn = vi.fn(() => new Promise<Response>(() => {})) as unknown as typeof fetch;
		const outcome = searchCatalogue(BASE, 'alpha', ctl.signal, fetchFn).catch((e) => e);
		ctl.abort();
		await expect(outcome).resolves.toMatchObject({ code: 'aborted' });
	});

	it('does not publish a response that resolves after caller abort', async () => {
		const ctl = new AbortController();
		let release!: () => void;
		const gate = new Promise<void>((r) => (release = r));
		const fetchFn = vi.fn(async (input: RequestInfo | URL) => {
			await gate;
			const type = new URL(String(input)).searchParams.get('type')!;
			return json(envelope(type, [{ mbid: A1, name: 'Late', score: 1 }]));
		}) as unknown as typeof fetch;
		const outcome = searchCatalogue(BASE, 'alpha', ctl.signal, fetchFn).then(
			(v) => ({ v }),
			(e) => ({ e })
		);
		ctl.abort();
		release();
		const result = await outcome;
		expect('v' in result).toBe(false);
		expect('e' in result && result.e.code).toBe('aborted');
	});

	it('rejects without fetching when already aborted and propagates abort to the transport', async () => {
		const pre = new AbortController();
		pre.abort();
		const none = vi.fn();
		await expect(
			searchCatalogue(BASE, 'alpha', pre.signal, none as unknown as typeof fetch)
		).rejects.toMatchObject({ code: 'aborted' });
		expect(none).not.toHaveBeenCalled();

		const ctl = new AbortController();
		const signals: AbortSignal[] = [];
		const fetchFn = vi.fn((_u: RequestInfo | URL, init?: RequestInit) => {
			signals.push(init!.signal!);
			return new Promise<Response>(() => {});
		}) as unknown as typeof fetch;
		const outcome = searchCatalogue(BASE, 'alpha', ctl.signal, fetchFn).catch((e) => e);
		ctl.abort();
		await outcome;
		expect(signals).toHaveLength(3);
		expect(signals.every((s) => s.aborted)).toBe(true);
	});
});
