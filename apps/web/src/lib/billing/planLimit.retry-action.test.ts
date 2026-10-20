import { describe, expect, it, vi } from 'vitest';
import { ApiClient, ApiError } from '../api/client.js';
import { onPaywall, paywallFromResponse, trackPaywallShown } from './planLimit.js';

describe('explicit paywall retry actions', () => {
	it.each(['3', '0', null])('publishes a UI retry without invoking it (wait %s)', (wait) => {
		const retry = vi.fn();
		const listener = vi.fn();
		const off = onPaywall(listener);
		try {
			const model = paywallFromResponse({
				status: 429,
				headers: new Headers(wait === null ? {} : { 'Retry-After': wait })
			}, retry);
			expect(listener).toHaveBeenCalledExactlyOnceWith(model);
			expect(listener.mock.calls[0][0].retry).toBe(retry);
			expect(retry).not.toHaveBeenCalled();
		} finally {
			off();
		}
	});

	it('never attaches retry to terminal models or creates a non-limit model', () => {
		const retry = vi.fn();
		expect(paywallFromResponse({ status: 402, headers: new Headers() }, retry))
			.not.toHaveProperty('retry');
		expect(paywallFromResponse({ status: 500, headers: new Headers() }, retry)).toBeNull();
		expect(retry).not.toHaveBeenCalled();
	});

	it('keeps the action and private body fields out of analytics', () => {
		const retry = vi.fn();
		const track = vi.fn();
		const model = paywallFromResponse({
			status: 429,
			headers: new Headers({ 'Retry-After': '3' }),
			body: { code: 'rate_limited', email: 'fixture@example.test', message: 'private-body-marker' }
		}, retry)!;
		trackPaywallShown(track, model);
		expect(track).toHaveBeenCalledExactlyOnceWith('usage_limit_paywall_shown', {
			kind: 'retryable', status: 429, code: 'rate_limited', retryAfterSeconds: 3
		});
		expect(retry).not.toHaveBeenCalled();
	});

	it('forwards the provider UI action, without automatically replaying the request', async () => {
		const retry = vi.fn();
		const fetchFn = vi.fn().mockResolvedValue(new Response(JSON.stringify({ code: 'rate_limited' }), {
			status: 429, headers: { 'content-type': 'application/json', 'Retry-After': '0' }
		}));
		const client = new ApiClient({ baseUrl: 'https://api.example.test', fetchFn });
		const failure = await client.testProviderConnection({
			host: 'news.example.test', port: 563, tls: true, allow_insecure_plaintext: false,
			username: 'fixture-reader', password: 'fixture-password', max_connections: 4
		}, retry).catch((error: unknown) => error);
		expect(failure).toBeInstanceOf(ApiError);
		expect((failure as ApiError).paywall?.retry).toBe(retry);
		expect(fetchFn).toHaveBeenCalledTimes(1);
		expect(retry).not.toHaveBeenCalled();
	});
});
