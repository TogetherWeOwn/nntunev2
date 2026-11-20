import { describe, expect, it, vi } from 'vitest';
import { ApiClient } from '../api/client.js';
import { importDeviceLikes, loadLikes, setTrackLike } from './likes.js';

const BASE = 'https://api.example.test';
const LIKES = `${BASE}/v1/me/likes`;
const MBID = 'fedcba98-7654-3210-fedc-ba9876543210';
const JOB = MBID.toUpperCase();

function mockClient(...responses: Response[]) {
	const fetchFn = vi.fn<typeof fetch>();
	for (const response of responses) fetchFn.mockResolvedValueOnce(response);
	return { client: new ApiClient({ baseUrl: BASE, fetchFn }), fetchFn };
}

function currentScope() {
	return { isCurrent: () => true, retireConfirmed: vi.fn() };
}

function importReceipt(added: number, already = 0) {
	return Response.json({ added, already, rejected: [] });
}

describe('server likes', () => {
	it('keeps server order and distinct kinds and preserves exact job IDs', async () => {
		const { client, fetchFn } = mockClient(Response.json({ items: [
			{ kind: 'track', target_id: JOB, created_at: 10 },
			{ kind: 'track', target_id: MBID, created_at: 9 },
			{ kind: 'release', target_id: MBID, created_at: 8 }
		] }));
		await expect(loadLikes(client)).resolves.toEqual([
			{ kind: 'track', targetId: JOB, createdAt: 10 },
			{ kind: 'track', targetId: MBID, createdAt: 9 },
			{ kind: 'release', targetId: MBID, createdAt: 8 }
		]);
		expect(fetchFn).toHaveBeenCalledWith(LIKES, expect.objectContaining({
			method: 'GET', credentials: 'include', headers: { accept: 'application/json' }
		}));
	});

	it('rejects over-cap lists, duplicates, and malformed typed targets', async () => {
		const like = { kind: 'track', target_id: 'Job_A', created_at: 9 };
		const { client } = mockClient(
			Response.json({ items: Array.from({ length: 501 }, (_, i) => ({ ...like, target_id: `Job_${i}` })) }),
			Response.json({ items: [like, like] }),
			Response.json({ items: [{ ...like, kind: 'release' }] })
		);
		for (let i = 0; i < 3; i++) await expect(loadLikes(client)).rejects.toThrow('Invalid likes list');
	});

	it('sends exact IDs on both methods and returns the acknowledged identity', async () => {
		const { client, fetchFn } = mockClient(
			Response.json({ target_id: JOB, liked_at: 1 }),
			Response.json({ target_id: MBID, liked: false })
		);
		await expect(setTrackLike(client, JOB, true)).resolves.toBe(JOB);
		await expect(setTrackLike(client, JOB, false)).resolves.toBe(MBID);
		expect(fetchFn).toHaveBeenNthCalledWith(1, `${LIKES}/track/${JOB}`, expect.objectContaining({
			method: 'PUT', credentials: 'include', body: '{}'
		}));
		expect(fetchFn).toHaveBeenNthCalledWith(2, `${LIKES}/track/${JOB}`, expect.objectContaining({
			method: 'DELETE', credentials: 'include'
		}));
		expect(fetchFn.mock.calls[1][1]?.body).toBeUndefined();
	});

	it('never sends an occurrence key or accepts a different opaque job receipt', async () => {
		const { client, fetchFn } = mockClient(Response.json({ target_id: 'job_a', liked_at: 1 }));
		await expect(setTrackLike(client, 'Job_A:2', true)).rejects.toThrow('Invalid track ID');
		expect(fetchFn).not.toHaveBeenCalled();
		await expect(setTrackLike(client, 'Job_A', true)).rejects.toThrow('Invalid like confirmation');
	});
});

describe('legacy likes import', () => {
	it('deduplicates exact IDs and retains rejected, ambiguous and invalid inputs', async () => {
		const prefix = 'J'.repeat(64);
		const { client, fetchFn } = mockClient(Response.json({
			added: 2, already: 1, rejected: [{ target_id: prefix, reason: 'not_member' }]
		}));
		const scope = currentScope();
		await importDeviceLikes(client, [JOB, '', JOB, MBID, `${prefix}accepted`, `${prefix}rejected`], scope);
		expect(fetchFn).toHaveBeenCalledOnce();
		expect(fetchFn).toHaveBeenCalledWith(`${LIKES}/import`, expect.objectContaining({ method: 'POST' }));
		expect(JSON.parse(String(fetchFn.mock.calls[0][1]?.body))).toEqual({
			items: [JOB, MBID, `${prefix}accepted`, `${prefix}rejected`].map((target_id) => ({ kind: 'track', target_id }))
		});
		expect(scope.retireConfirmed).toHaveBeenCalledExactlyOnceWith([JOB, MBID]);
	});

	it('retires complete bounded batches but keeps unconfirmed data for retry', async () => {
		const ids = Array.from({ length: 53 }, (_, i) => `Job_${i}`);
		const { client } = mockClient(importReceipt(0, 50), Response.json({}, { status: 503 }), importReceipt(2, 1));
		const scope = currentScope();
		await expect(importDeviceLikes(client, ids, scope)).rejects.toMatchObject({ status: 503 });
		expect(scope.retireConfirmed).toHaveBeenCalledExactlyOnceWith(ids.slice(0, 50));
		await importDeviceLikes(client, ids.slice(50), scope);
		expect(scope.retireConfirmed).toHaveBeenLastCalledWith(ids.slice(50));
	});

	it('does not trust incomplete counts or mismatched rejection identities', async () => {
		const { client } = mockClient(importReceipt(0), Response.json({
			added: 0, already: 0, rejected: [{ target_id: 'another', reason: 'not_member' }]
		}));
		const scope = currentScope();
		for (let i = 0; i < 2; i++) {
			await expect(importDeviceLikes(client, ['Job_A'], scope)).rejects.toThrow('Invalid likes import confirmation');
		}
		expect(scope.retireConfirmed).not.toHaveBeenCalled();
	});

	it('ignores a late receipt after an auth transition and does not send the next batch', async () => {
		const { client, fetchFn } = mockClient();
		let finish!: (response: Response) => void;
		fetchFn.mockReturnValueOnce(new Promise((resolve) => { finish = resolve; }));
		let current = true;
		const retireConfirmed = vi.fn();
		const ids = Array.from({ length: 51 }, (_, i) => `Job_${i}`);
		const importing = importDeviceLikes(client, ids, { isCurrent: () => current, retireConfirmed });
		current = false;
		finish(importReceipt(50));
		await importing;
		await importDeviceLikes(client, ids, { isCurrent: () => current, retireConfirmed });
		expect(fetchFn).toHaveBeenCalledOnce();
		expect(retireConfirmed).not.toHaveBeenCalled();
	});
});
