import { describe, expect, it } from 'vitest';
import { ApiClient, ApiError } from '../api/client.js';
import { confirmWithPasskey, loadStatus, unlink } from './flow.js';

type Call = { url: string; method: string; credentials?: RequestCredentials; headers?: unknown; body?: unknown };

function mockClient(respond: (call: Call) => Response | Promise<Response>) {
	const calls: Call[] = [];
	const fetchFn = (async (input: RequestInfo | URL, init?: RequestInit) => {
		const call: Call = {
			url: String(input),
			method: init?.method ?? 'GET',
			credentials: init?.credentials,
			headers: init?.headers,
			body: init?.body
		};
		calls.push(call);
		return respond(call);
	}) as typeof fetch;
	return { calls, client: new ApiClient({ baseUrl: 'https://api.example.test', fetchFn }) };
}

const envelope = (status: number, code: string) =>
	new Response(JSON.stringify({ code, message: 'm', retryable: false }), { status });

describe('ApiClient ListenBrainz helpers', () => {
	it('GETs a token-free status with credentials and no body or authorization', async () => {
		const { calls, client } = mockClient(() =>
			Response.json({ linked: true, username: 'alice', token: 'ignored-extra' })
		);
		const status = await client.getListenBrainzStatus();
		expect(status).toEqual({ linked: true, username: 'alice' });
		expect(calls).toHaveLength(1);
		expect(calls[0]).toMatchObject({
			url: 'https://api.example.test/v1/me/listenbrainz',
			method: 'GET',
			credentials: 'include'
		});
		expect(calls[0].body).toBeUndefined();
		expect(JSON.stringify(calls[0].headers)).not.toMatch(/authorization/i);
	});

	it('parses the unlinked status', async () => {
		const { client } = mockClient(() => Response.json({ linked: false }));
		expect(await client.getListenBrainzStatus()).toEqual({ linked: false });
	});

	it.each([{}, { linked: true }, { linked: true, username: '' }, { linked: 'yes' }, [], null])(
		'rejects malformed status body %j with a typed error',
		async (body) => {
			const { client } = mockClient(() => Response.json(body));
			await expect(client.getListenBrainzStatus()).rejects.toBeInstanceOf(ApiError);
		}
	);

	it('DELETEs once and returns the unlinked status', async () => {
		const { calls, client } = mockClient(() => Response.json({ linked: false }));
		expect(await client.unlinkListenBrainz()).toEqual({ linked: false });
		expect(calls).toHaveLength(1);
		expect(calls[0]).toMatchObject({
			url: 'https://api.example.test/v1/me/listenbrainz',
			method: 'DELETE',
			credentials: 'include'
		});
		expect(calls[0].body).toBeUndefined();
	});

	it('surfaces reauth_required as a typed 401 error', async () => {
		const { client } = mockClient(() => envelope(401, 'reauth_required'));
		const error = await client.unlinkListenBrainz().catch((e: unknown) => e);
		expect(error).toBeInstanceOf(ApiError);
		expect((error as ApiError).code).toBe('reauth_required');
	});
});

describe('loadStatus', () => {
	it('maps linked and unlinked', async () => {
		expect(
			await loadStatus(mockClient(() => Response.json({ linked: true, username: 'bob' })).client)
		).toEqual({ kind: 'linked', username: 'bob' });
		expect(await loadStatus(mockClient(() => Response.json({ linked: false })).client)).toEqual({
			kind: 'unlinked'
		});
	});

	it('maps a generic 401 to signed out', async () => {
		const { client } = mockClient(() => envelope(401, 'unauthorized'));
		expect(await loadStatus(client)).toEqual({ kind: 'signed_out' });
	});

	it.each([500, 503])('maps %i to the generic error', async (status) => {
		const { client } = mockClient(() => envelope(status, 'internal_error'));
		expect(await loadStatus(client)).toEqual({ kind: 'error' });
	});

	it('maps a network failure to the generic error', async () => {
		const { client } = mockClient(() => {
			throw new TypeError('network down');
		});
		expect(await loadStatus(client)).toEqual({ kind: 'error' });
	});
});

describe('unlink', () => {
	it('sends exactly one DELETE and reports unlinked', async () => {
		const { calls, client } = mockClient(() => Response.json({ linked: false }));
		expect(await unlink(client)).toEqual({ kind: 'unlinked' });
		expect(calls.map((c) => c.method)).toEqual(['DELETE']);
	});

	it('treats a 200 that still says linked as a failure', async () => {
		const { client } = mockClient(() => Response.json({ linked: true, username: 'alice' }));
		expect(await unlink(client)).toEqual({ kind: 'failed' });
	});

	it('reports reauth_required once, without retrying by itself', async () => {
		const { calls, client } = mockClient(() => envelope(401, 'reauth_required'));
		expect(await unlink(client)).toEqual({ kind: 'reauth_required' });
		expect(calls).toHaveLength(1);
	});

	it('reports a second reauth_required after the retry as reauth_again', async () => {
		const { calls, client } = mockClient(() => envelope(401, 'reauth_required'));
		expect(await unlink(client, { afterReauth: true })).toEqual({ kind: 'reauth_again' });
		expect(calls).toHaveLength(1);
	});

	it('maps a generic 401 to signed out', async () => {
		const { client } = mockClient(() => envelope(401, 'unauthorized'));
		expect(await unlink(client)).toEqual({ kind: 'signed_out' });
	});

	it('maps account_deleting (409)', async () => {
		const { client } = mockClient(() => envelope(409, 'account_deleting'));
		expect(await unlink(client)).toEqual({ kind: 'account_deleting' });
	});

	it.each([500, 503])('maps %i to failed', async (status) => {
		const { client } = mockClient(() => envelope(status, 'internal_error'));
		expect(await unlink(client)).toEqual({ kind: 'failed' });
	});

	it('maps a network failure to failed', async () => {
		const { client } = mockClient(() => {
			throw new TypeError('network down');
		});
		expect(await unlink(client)).toEqual({ kind: 'failed' });
	});
});

describe('confirmWithPasskey', () => {
	it('reports dismissed and makes no finish call when the signal is already aborted', async () => {
		const { calls, client } = mockClient(() =>
			Response.json({ challenge: 'YQ', rp_id: 'example.test', allow_credentials: [] })
		);
		const controller = new AbortController();
		controller.abort();
		expect(await confirmWithPasskey(client, controller.signal)).toBe('dismissed');
		expect(calls.filter((c) => c.url.endsWith('/reauthenticate/finish'))).toHaveLength(0);
	});

	it('reports failed when the start call fails', async () => {
		const { client } = mockClient(() => envelope(401, 'unauthorized'));
		expect(await confirmWithPasskey(client, new AbortController().signal)).toBe('failed');
	});
});
