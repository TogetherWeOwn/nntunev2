import { afterEach, expect, it, vi } from 'vitest';

import { validBinding, verifyLibrarySession } from './session-verification.js';

vi.mock('$app/env/public', () => ({ PUBLIC_API_BASE_URL: 'https://api.example.test' }));

const binding = 'A'.repeat(43);
const otherBinding = 'E'.repeat(43);
const verify = () => {
	const { signal } = new AbortController();
	return verifyLibrarySession(binding, signal, { generation: 'test', signal, isCurrent: () => true, run: async (work) => work() });
};

function reply(response: Response) {
	const fetch = vi.fn().mockResolvedValue(response);
	vi.stubGlobal('fetch', fetch);
	return fetch;
}

afterEach(() => vi.unstubAllGlobals());

it('accepts only canonical unpadded base64url SHA-256 labels', () => {
	const prefix = '_-'.repeat(21);
	for (const last of 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_') {
		expect(validBinding(prefix + last), last).toBe('AEIMQUYcgkosw048'.includes(last));
	}
	for (const invalid of [null, 123, '', prefix, `${binding}=`, `+${binding.slice(1)}`]) {
		expect(validBinding(invalid)).toBe(false);
	}
});

it.each([[binding, true], [otherBinding, false]] as const)(
	'compares the verified label %s with the expected binding',
	async (session_binding, matches) => {
		const fetch = reply(Response.json({ session_binding }));
		await expect(verify()).resolves.toBe(matches);
		expect(fetch).toHaveBeenCalledExactlyOnceWith(
			'https://api.example.test/v1/me/likes/session',
			expect.objectContaining({ method: 'GET', credentials: 'include' })
		);
	}
);

it.each([401, 403])('treats HTTP %i as refusal', async (status) => {
	reply(new Response(null, { status }));
	await expect(verify()).resolves.toBe(false);
});

it('propagates a temporary failure instead of reporting refusal', async () => {
	reply(new Response(null, { status: 503 }));
	await expect(verify()).rejects.toMatchObject({ name: 'ApiError', status: 503 });
});

it.each([{}, { session_binding: `${binding.slice(1)}B` }])(
	'rejects a successful response with no canonical binding (%j)',
	async (result) => {
		reply(Response.json(result));
		await expect(verify()).rejects.toThrow('Missing verified session binding');
	}
);
