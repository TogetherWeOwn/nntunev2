import { describe, expect, it } from 'vitest';

import { notReadyResponse, readyResponse } from './ready.js';

describe('readyResponse', () => {
	it('returns 200 with {ok:true}', async () => {
		const res = readyResponse();

		expect(res.status).toBe(200);
		await expect(res.json()).resolves.toEqual({ ok: true });
	});

	it('sends Cache-Control: no-store', () => {
		expect(readyResponse().headers.get('cache-control')).toBe('no-store');
	});
});

describe('notReadyResponse', () => {
	it('returns 503 with {ok:false} and a Retry-After header', async () => {
		const res = notReadyResponse(5);

		expect(res.status).toBe(503);
		expect(res.headers.get('retry-after')).toBe('5');
		await expect(res.json()).resolves.toEqual({ ok: false });
	});

	it('sends Cache-Control: no-store alongside Retry-After', () => {
		const res = notReadyResponse(5);

		expect(res.headers.get('cache-control')).toBe('no-store');
		expect(res.headers.get('retry-after')).toBe('5');
	});
});
