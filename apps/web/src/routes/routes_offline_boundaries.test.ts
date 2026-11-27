import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { variables } from '../env.js';
import { handle } from '../hooks.server.js';
import { captureAuthFence } from '#lib/auth-fence.js';
import { createQueue } from '#lib/player/queue.js';
import { load as loadBilling } from './account/billing/+page.js';
import { load as loadSecurity } from './account/security/+page.js';
import { GET as health } from './api/health/+server.js';
import { GET as ready } from './api/ready/+server.js';
import { GET as build } from './api/build/+server.js';
import { GET as securityTxt } from './.well-known/security.txt/+server.js';
import { GET as manifest } from './manifest.webmanifest/+server.js';
import { createRecentListensReader } from './home/recent-listens.js';
import { createFavoritesExport } from './library/favoritesExport.js';
import { projectInteractionIds } from './library/interactionSelection.js';
import { selectedRowRecoveryIds } from './library/selectedRowRecovery.js';

vi.mock('#lib/auth-fence.js', () => ({ captureAuthFence: vi.fn() }));
vi.mock('$app/env/public', () => ({ PUBLIC_API_BASE_URL: 'https://api.example.test' }));

const activeSubscription = {
	state: 'active',
	planId: 'pro',
	status: 'active',
	currentPeriodEnd: null,
	cancelAtPeriodEnd: false
};
const usage = { period: '2026-11', streamSecondsMonth: 1, resetsAt: '2026-12-01T00:00:00Z' };

beforeEach(() => {
	vi.mocked(captureAuthFence).mockResolvedValue({
		generation: 'test-session',
		signal: new AbortController().signal,
		isCurrent: () => true,
		run: async (work) => work()
	});
});

afterEach(() => vi.unstubAllGlobals());

describe('API base URL environment schema', () => {
	const validate = variables.PUBLIC_API_BASE_URL.schema['~standard'].validate;

	it.each([
		'/relative',
		'https:api.example.test',
		'https:///api.example.test',
		'https://api.example.test\\other',
		'https://api.example.test/path?',
		'https://api.example.test/path#',
		'https://user@api.example.test',
		'https://@api.example.test',
		'https://api.example.test\t',
		'https://api.example.test:99999',
		'http://api.example.test'
	])('rejects malformed or insecure configuration %j', async (value) => {
		expect(await validate(value)).toEqual({ issues: [{ message: expect.any(String) }] });
	});

	it.each([undefined, '', 'https://api.example.test:8443/prefix/', 'http://[::1]:8787'])(
		'preserves supported API bases %j',
		async (value) => expect(await validate(value)).toEqual({ value: value ?? '' })
	);
});

const event = {} as Parameters<typeof handle>[0]['event'];
const respond = (response: Response) => handle({ event, resolve: async () => response });

it('adds security headers to immutable redirects and preserves their location', async () => {
	const response = await respond(Response.redirect('https://app.example.test/login', 307));
	expect(response.status).toBe(307);
	expect(response.headers.get('location')).toBe('https://app.example.test/login');
	expect(response.headers.get('referrer-policy')).toBe('no-referrer');
	expect(response.headers.get('x-content-type-options')).toBe('nosniff');
});

it('preserves route caching and the generated CSP while overriding static headers', async () => {
	const csp = "script-src 'nonce-test'";
	const response = await respond(new Response('page', { headers: {
		'content-security-policy': csp,
		'cache-control': 'private, no-store',
		'referrer-policy': 'unsafe-url'
	} }));
	expect(response.headers.get('content-security-policy')).toBe(csp);
	expect(response.headers.get('cache-control')).toBe('private, no-store');
	expect(response.headers.get('referrer-policy')).toBe('no-referrer');
	await expect(response.text()).resolves.toBe('page');
});

it('sets explicit cache policies on operational metadata and public static endpoints', async () => {
	for (const { get, cache, type } of [
		{ get: health, cache: 'no-store', type: 'application/json' },
		{ get: ready, cache: 'no-store', type: 'application/json' },
		{ get: build, cache: 'no-store', type: 'application/json' },
		{ get: manifest, cache: 'public, max-age=3600', type: 'application/manifest+json' },
		{ get: securityTxt, cache: 'no-cache', type: 'text/plain' }
	]) {
		const response = await get({} as never);
		expect(response.status).toBe(200);
		expect(response.headers.get('content-type')?.split(';')[0]).toBe(type);
		expect(response.headers.get('cache-control')).toBe(cache);
	}
});

it.each([null, {}, { state: 'active' }])('warns on a malformed security snapshot %j', async (body) => {
	const fetch = vi.fn(async () => Response.json(body));
	await expect(loadSecurity({ fetch } as Parameters<typeof loadSecurity>[0]))
		.resolves.toEqual({ sessionCheckFailed: true });
	expect(fetch).toHaveBeenCalledOnce();
});

it('checks the configured API without caching or following redirects when Web Locks are unavailable', async () => {
	vi.mocked(captureAuthFence).mockResolvedValueOnce(undefined);
	vi.stubGlobal('window', {});
	vi.stubGlobal('navigator', {});
	const fetch = vi.fn(async () => Response.json(activeSubscription));
	await expect(loadSecurity({ fetch } as Parameters<typeof loadSecurity>[0]))
		.resolves.toEqual({ sessionCheckFailed: false });
	expect(fetch).toHaveBeenCalledExactlyOnceWith('https://api.example.test/v1/me/subscription', expect.objectContaining({
		credentials: 'include',
		cache: 'no-store',
		redirect: 'error'
	}));
});

it.each(['/v1/me/subscription', '/v1/me/usage'])(
	'discards both billing payloads when %s returns 401',
	async (deniedPath) => {
		const fetch = vi.fn(async (path: string) => path === deniedPath
			? new Response(null, { status: 401 })
			: Response.json(path.endsWith('/subscription') ? activeSubscription : usage));
		await expect(loadBilling({ fetch } as Parameters<typeof loadBilling>[0])).resolves.toMatchObject({
			signedOut: true,
			planUnknown: true,
			canUpgrade: false,
			subscription: null,
			usage: null,
			usageStatus: null
		});
	}
);

it.each([
	{ ...activeSubscription, state: 'no_subscription' },
	{ ...activeSubscription, status: 'canceled' },
	{ ...activeSubscription, state: 'lapsed', status: null }
])('rejects a %j billing snapshot with contradictory state and live fields', async (subscription) => {
	const fetch = vi.fn(async (path: string) =>
		Response.json(path.endsWith('/subscription') ? subscription : usage)
	);
	await expect(loadBilling({ fetch } as Parameters<typeof loadBilling>[0])).resolves.toMatchObject({
		hadFetchError: true,
		planUnknown: true,
		canUpgrade: false,
		subscription: null,
		usage,
		usageStatus: null
	});
});

it.each([
	['é'.repeat(64), 'ready'],
	['é'.repeat(63) + 'abc', 'error'],
	['one\u0000', 'error']
])('validates the track reference before publishing history (%j)', async (track_id, kind) => {
	const row = { playback_id: '10000000-0000-0000-0000-000000000000', track_id: 'one', event: 'start', recorded_at: 5 };
	vi.stubGlobal('fetch', vi.fn(async () => Response.json({
		retention: 'account_lifetime',
		next_cursor: null,
		events: [row, { ...row, event: 'complete', track_id }]
	})));
	const publish = vi.fn();
	await createRecentListensReader('', publish).load();
	expect(publish).toHaveBeenCalledTimes(2);
	expect(publish).toHaveBeenLastCalledWith(kind === 'ready'
		? { kind, events: [expect.objectContaining({ track: row.track_id }), expect.objectContaining({ track: track_id })] }
		: { kind });
});

it('refuses coercible non-string track IDs before clipboard or download work', async () => {
	const state = createQueue([{ id: 123 as unknown as string, title: 'Title', artist: '', streamUrl: '' }]);
	const writeText = vi.fn();
	const createElement = vi.fn(() => { throw new Error('unexpected DOM access'); });
	const setFeedback = vi.fn();
	vi.stubGlobal('navigator', { clipboard: { writeText } });
	vi.stubGlobal('document', { createElement });
	const exporter = createFavoritesExport({
		isEmpty: () => false,
		snapshot: () => state,
		currentKey: () => 'key',
		clearFeedback: () => {},
		setFeedback
	});

	await exporter.copy();
	expect(writeText).not.toHaveBeenCalled();
	exporter.downloadM3U();
	expect(createElement).not.toHaveBeenCalled();
	expect(setFeedback.mock.calls).toEqual(Array.from({ length: 2 }, () =>
		['key', 'Could not export favorites: a favorite has an unsafe track id.']
	));
});

it('refuses an interaction whose anchor is no longer rendered', () => {
	expect(projectInteractionIds('gone', [], [{ id: 'present' }])).toEqual([]);
	expect(projectInteractionIds('gone', ['gone', 'present'], [{ id: 'present' }])).toEqual([]);
});

it('excludes the removed row from focus recovery even when its ID is repeated', () => {
	expect(selectedRowRecoveryIds(['gone', 'gone', 'survivor'], 'gone', ['gone', 'survivor']))
		.toEqual({ nextId: 'survivor', previousId: null });
});
