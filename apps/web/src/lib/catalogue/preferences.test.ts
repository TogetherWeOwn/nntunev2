import { beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiClient } from '../api/client.js';
import { collectionPreference, type PreferenceKind } from './preferences.js';
import { createCollectionPreference } from './preference.svelte.js';

const auth = vi.hoisted(() => ({ generation: 0 }));
vi.mock('./auth.svelte.js', () => ({ catalogueAuthGeneration: () => auth.generation }));
const mbid = 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa';
const baseUrl = 'https://api.example.test';
const at = '2026-11-08T00:00:00Z';
const body = (kind: PreferenceKind, enabled: boolean) => kind === 'albums'
	? { saved: enabled, savedAt: enabled ? at : null } : { followed: enabled, followedAt: enabled ? at : null };
const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status, headers: { 'content-type': 'application/json' } });
beforeEach(() => { auth.generation = 0; });

for (const kind of ['albums', 'artists'] as const) {
	describe(`${kind} preference contract`, () => {
		it('reads only the exact normalized entity and uses bodiless credentialed mutations', async () => {
			const fetchFn = vi.fn<typeof fetch>().mockResolvedValueOnce(json(body(kind, false)))
				.mockResolvedValueOnce(json(body(kind, true))).mockResolvedValueOnce(json(body(kind, false)));
			const client = new ApiClient({ baseUrl, fetchFn });
			expect(await collectionPreference(client, kind, mbid.toUpperCase())).toEqual({ value: false, failure: null });
			expect(await collectionPreference(client, kind, mbid, true)).toEqual({ value: true, failure: null });
			expect(await collectionPreference(client, kind, mbid, false)).toEqual({ value: false, failure: null });
			for (const [index, method] of ['GET', 'PUT', 'DELETE'].entries()) {
				expect(fetchFn.mock.calls[index][0]).toBe(`${baseUrl}/v1/me/library/${kind}/${mbid}`);
				expect(fetchFn.mock.calls[index][1]).toMatchObject({ method, credentials: 'include' });
				expect(fetchFn.mock.calls[index][1]?.body).toBeUndefined();
			}
		});

		it('rejects malformed IDs before any request', async () => {
			const fetchFn = vi.fn<typeof fetch>();
			const client = new ApiClient({ baseUrl, fetchFn });
			for (const id of ['', '../admin', `${mbid}/art`, 'invalid']) {
				expect((await collectionPreference(client, kind, id, true)).failure).toBe('not-found');
			}
			expect(fetchFn).not.toHaveBeenCalled();
		});

		it.each([[401, 'unauthorized', 'signed-out'], [404, 'not_found', 'not-found'],
			[409, 'account_deleting', 'account-deleting'], [409, 'other', 'unavailable'], [503, 'unavailable', 'unavailable']] as const)
		('keeps %i %s distinct without inventing a boolean', async (status, code, failure) => {
			const fetchFn = vi.fn<typeof fetch>().mockResolvedValue(json({ code, message: 'private diagnostics', retryable: false }, status));
			expect(await collectionPreference(new ApiClient({ baseUrl, fetchFn }), kind, mbid)).toEqual({ value: null, failure });
		});

		it('refuses wrong shapes, inconsistent timestamps and opposite mutation results', async () => {
			const flag = kind === 'albums' ? 'saved' : 'followed';
			const stamp = kind === 'albums' ? 'savedAt' : 'followedAt';
			for (const value of [null, {}, [], { [flag]: 'true', [stamp]: at }, { [flag]: true, [stamp]: null },
				{ [flag]: false, [stamp]: at }, { [flag]: true, [stamp]: '2026-02-31T00:00:00Z' },
				{ [flag]: true, [stamp]: 'yesterday' }, body(kind, false)]) {
				const fetchFn = vi.fn<typeof fetch>().mockResolvedValue(json(value));
				expect(await collectionPreference(new ApiClient({ baseUrl, fetchFn }), kind, mbid, true))
					.toEqual({ value: null, failure: 'unavailable' });
			}
		});
	});
}

describe('route-owned preference state', () => {
	it('serializes activations, confirms from the server and reconciles a failed write before another', async () => {
		let finish!: (response: Response) => void;
		const fetchFn = vi.fn<typeof fetch>().mockResolvedValueOnce(json(body('albums', false)))
			.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }))
			.mockResolvedValueOnce(json(body('albums', true)))
			.mockResolvedValueOnce(json(body('albums', false)));
		const state = createCollectionPreference('albums', { baseUrl, fetchFn });
		state.load(mbid, 0);
		await vi.waitFor(() => expect(state.value).toBe(false));
		const write = state.toggle();
		void state.toggle();
		expect(state.value).toBe(false);
		expect(state.pending).toBe(true);
		expect(fetchFn).toHaveBeenCalledTimes(2);
		finish(json({ code: 'service_unavailable', message: 'diagnostic', retryable: true }, 503));
		await write;
		expect(state.value).toBe(false);
		expect(state.failure).toBe('unavailable');
		await state.toggle();
		expect(fetchFn).toHaveBeenCalledTimes(2);
		await state.retry();
		expect(fetchFn.mock.calls[2][1]?.method).toBe('GET');
		expect(state.value).toBe(true);
		await state.toggle();
		expect(fetchFn.mock.calls[3][1]?.method).toBe('DELETE');
		expect(state.value).toBe(false);
		state.clear();
	});

	it('clears and aborts on target change, refusing a response from a transport that ignores abort', async () => {
		let finish!: (response: Response) => void;
		const fetchFn = vi.fn<typeof fetch>().mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }))
			.mockResolvedValueOnce(json(body('artists', false)));
		const state = createCollectionPreference('artists', { baseUrl, fetchFn });
		const old = state.load(mbid, 0);
		const oldSignal = fetchFn.mock.calls[0][1]?.signal;
		await state.load('bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb', 0);
		expect(oldSignal?.aborted).toBe(true);
		expect(state.value).toBe(false);
		finish(json(body('artists', true)));
		await old;
		expect(state.value).toBe(false);
		state.clear();
		expect(state.value).toBeNull();
	});

	it('fences the mutation before dispatch and publication after an auth-generation change', async () => {
		let finish!: (response: Response) => void;
		const fetchFn = vi.fn<typeof fetch>().mockResolvedValueOnce(json(body('albums', false)))
			.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
		const state = createCollectionPreference('albums', { baseUrl, fetchFn });
		state.load(mbid, 0);
		await vi.waitFor(() => expect(state.value).toBe(false));
		const write = state.toggle();
		auth.generation += 1;
		finish(json(body('albums', true)));
		await write;
		expect(state.value).toBe(false);
		state.clear();
		state.load(mbid, 0);
		await state.toggle();
		expect(state.value).toBeNull();
		expect(fetchFn).toHaveBeenCalledTimes(2);
		state.clear();
	});
});
