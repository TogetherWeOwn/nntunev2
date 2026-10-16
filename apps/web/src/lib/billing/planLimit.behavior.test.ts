/**
 * Behavioral coverage for planLimit.ts beyond the frozen acceptance file
 * (planLimit.test.ts): non-limit statuses, Retry-After date form, usage
 * naming from body figures, and the display-listener plumbing.
 */

import { afterEach, describe, expect, it, vi } from 'vitest';
import {
	formatCountdown,
	onPaywall,
	parseRetryAfter,
	paywallFromResponse,
	toPaywallModel,
	type PaywallModel
} from './planLimit.js';

afterEach(() => vi.unstubAllGlobals());

describe('toPaywallModel non-limit statuses', () => {
	it('returns null for 200/401/404/500 so existing error handling stands', () => {
		for (const status of [200, 401, 404, 500]) {
			expect(toPaywallModel({ status, headers: new Headers() })).toBeNull();
		}
	});
});

describe('usage naming', () => {
	it('names exact figures when the body carries used/limit/tier', () => {
		const model = toPaywallModel({
			status: 402,
			headers: new Headers(),
			body: { code: 'plan_limit_exceeded', used_bytes: 2048, limit_bytes: 2048, plan: 'free' }
		});
		expect(model?.detail).toMatch(/2\D*048 of 2\D*048/);
		expect(model?.detail).toContain('free');
		expect(model?.code).toBe('plan_limit_exceeded');
	});

	it('does not treat a prose error message as a code', () => {
		const model = toPaywallModel({
			status: 429,
			headers: new Headers(),
			body: { error: 'You are doing that too often, please wait' }
		});
		expect(model?.code).toBeNull();
	});
});

describe('parseRetryAfter', () => {
	it('parses delta-seconds and HTTP-date; rejects garbage; clamps past dates', () => {
		const now = Date.parse('2026-10-15T12:00:00Z');
		const delta = new Headers({ 'Retry-After': '45' });
		expect(parseRetryAfter(delta, now)).toBe(45);

		const date = new Headers({ 'Retry-After': 'Thu, 15 Oct 2026 12:00:30 GMT' });
		expect(parseRetryAfter(date, now)).toBe(30);

		const past = new Headers({ 'Retry-After': 'Thu, 15 Oct 2026 11:59:00 GMT' });
		expect(parseRetryAfter(past, now)).toBe(0);

		expect(parseRetryAfter(new Headers({ 'Retry-After': 'soon' }), now)).toBeNull();
		expect(parseRetryAfter(new Headers(), now)).toBeNull();
	});

	it('429 without Retry-After is retryable with no countdown', () => {
		const model = toPaywallModel({ status: 429, headers: new Headers() });
		expect(model?.kind).toBe('retryable');
		expect(model?.retryAfterSeconds).toBeNull();
		expect(model?.countdownText).toBeNull();
	});
});

describe('formatCountdown', () => {
	it('covers seconds, minutes, and the zero state', () => {
		expect(formatCountdown(45)).toBe('Try again in 45 seconds.');
		expect(formatCountdown(1)).toBe('Try again in 1 second.');
		expect(formatCountdown(125)).toBe('Try again in 2 minutes 5 seconds.');
		expect(formatCountdown(120)).toBe('Try again in 2 minutes.');
		expect(formatCountdown(0)).toBe('You can try again now.');
	});
});

describe('paywall display plumbing', () => {
	it('replays a limit response received before the first subscriber', () => {
		vi.stubGlobal('window', {});
		const seen: PaywallModel[] = [];
		const model = paywallFromResponse({
			status: 429,
			headers: new Headers({ 'Retry-After': '45' })
		});

		expect(model?.kind).toBe('retryable');
		expect(seen).toHaveLength(0);

		const off = onPaywall((pending) => seen.push(pending));
		expect(seen).toEqual([model]);
		off();
	});

	it('notifies listeners for limits only, survives a throwing listener, unsubscribes', () => {
		const seen: PaywallModel[] = [];
		const offBad = onPaywall(() => {
			throw new Error('listener boom');
		});
		const off = onPaywall((model) => seen.push(model));

		const model = paywallFromResponse({ status: 402, headers: new Headers() });
		expect(model).not.toBeNull();
		expect(seen).toHaveLength(1);
		expect(seen[0]?.kind).toBe('terminal');

		expect(paywallFromResponse({ status: 500, headers: new Headers() })).toBeNull();
		expect(seen).toHaveLength(1);

		off();
		offBad();
		vi.stubGlobal('window', {});
		paywallFromResponse({ status: 402, headers: new Headers() });
		expect(seen).toHaveLength(1);
		const pending: PaywallModel[] = [];
		const offPending = onPaywall((model) => pending.push(model));
		expect(pending[0]?.status).toBe(402);
		offPending();
	});
});
