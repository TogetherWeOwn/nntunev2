/**
 * Protected acceptance tests: bound Retry-After waits before the paywall
 * countdown text and its per-second timer ever see them.
 *
 * This acceptance test is frozen; change it only through its owning work
 * item. The Builder builds on this branch and must not edit this file.
 *
 * Contract for `src/lib/billing/planLimit.ts`:
 *
 * - There is a single named upper bound, exported as
 *   `MAX_RETRY_AFTER_SECONDS`, equal to 3600 (one hour).
 * - `parseRetryAfter(headers, nowMs)` clamps EVERY wait above that bound to
 *   exactly the bound, for both header forms:
 *   - delta-seconds above the cap (e.g. `3601`, `86400`),
 *   - a 400-digit delta (`'9'.repeat(400)`) whose `parseInt` overflows to
 *     `Infinity` today,
 *   - a far-future HTTP-date (and a date only one second past the cap).
 *   The returned wait is therefore always finite, so countdown text derived
 *   from it never renders `Infinity`/`NaN`, and a per-second decrement of the
 *   wait (the paywall modal's timer discipline) always terminates at the
 *   try-again-now state (`formatCountdown(0)`).
 * - Waits at or below the cap are returned unchanged.
 * - Existing semantics are preserved exactly: delta `0` -> `0`, a past
 *   HTTP-date -> `0`, a missing header -> `null`, unparseable -> `null`.
 * - `toPaywallModel` on a 429 carries the capped wait and finite countdown
 *   text; no automatic retry/mutation behavior is added.
 *
 * Fake clocks/timers only — no real sleeps. The new export is read through a
 * dynamic import so static typecheck stays clean while it is missing; the
 * runtime assertion fails (RED) until the Builder exports it.
 */

import { afterEach, describe, expect, it, vi } from 'vitest';
import { formatCountdown, parseRetryAfter, toPaywallModel } from './planLimit.js';

/** The one-hour cap this contract fixes. The Builder must match it exactly. */
const CAP = 3600;

const NOW = Date.parse('2026-10-15T12:00:00Z');

const HUGE_DELTA = '9'.repeat(400);
const FAR_FUTURE_DATE = 'Fri, 15 Oct 2126 12:00:00 GMT';

function retryAfter(value: string): Headers {
	return new Headers({ 'Retry-After': value });
}

function httpDateSecondsFromNow(seconds: number): string {
	return new Date(NOW + seconds * 1000).toUTCString();
}

/**
 * Drive the paywall timer discipline (decrement once per second, stop at
 * zero) under fake timers and report whether it reached try-again-now.
 * An unbounded/Infinity start can never reach zero within CAP ticks.
 */
function terminatesAtTryAgainNow(startSeconds: number | null): boolean {
	vi.useFakeTimers();
	try {
		let remaining = startSeconds ?? Number.POSITIVE_INFINITY;
		const timer = setInterval(() => {
			remaining -= 1;
			if (remaining <= 0) clearInterval(timer);
		}, 1000);
		vi.advanceTimersByTime(CAP * 1000);
		return remaining <= 0 && formatCountdown(0) === 'You can try again now.';
	} finally {
		vi.clearAllTimers();
		vi.useRealTimers();
	}
}

afterEach(() => {
	vi.clearAllTimers();
	vi.useRealTimers();
});

describe('Retry-After cap export', () => {
	it('exports MAX_RETRY_AFTER_SECONDS as exactly 3600', async () => {
		const specifier = './planLimit.js';
		// eslint-disable-next-line @typescript-eslint/no-explicit-any
		const planLimit: any = await import(specifier);
		expect(planLimit.MAX_RETRY_AFTER_SECONDS).toBe(CAP);
	});
});

describe('parseRetryAfter clamps waits above the cap', () => {
	it.each(['3601', '86400', '100000'])('clamps delta-seconds %s to the cap', (value) => {
		expect(parseRetryAfter(retryAfter(value), NOW)).toBe(CAP);
	});

	it('clamps a 400-digit delta (parseInt overflow) to the finite cap', () => {
		const wait = parseRetryAfter(retryAfter(HUGE_DELTA), NOW);
		expect(wait).toBe(CAP);
		expect(Number.isFinite(wait)).toBe(true);
	});

	it('clamps a far-future HTTP-date to the cap', () => {
		expect(parseRetryAfter(retryAfter(FAR_FUTURE_DATE), NOW)).toBe(CAP);
	});

	it('clamps an HTTP-date one second past the cap to the cap', () => {
		expect(parseRetryAfter(retryAfter(httpDateSecondsFromNow(CAP + 1)), NOW)).toBe(CAP);
	});
});

describe('parseRetryAfter preserves at/below-cap and legacy semantics', () => {
	it('returns waits at or below the cap unchanged', () => {
		expect(parseRetryAfter(retryAfter('45'), NOW)).toBe(45);
		expect(parseRetryAfter(retryAfter('3599'), NOW)).toBe(3599);
		expect(parseRetryAfter(retryAfter('3600'), NOW)).toBe(CAP);
		expect(parseRetryAfter(retryAfter(httpDateSecondsFromNow(30)), NOW)).toBe(30);
		expect(parseRetryAfter(retryAfter(httpDateSecondsFromNow(CAP)), NOW)).toBe(CAP);
	});

	it('preserves 0 / past-date / missing / unparseable semantics', () => {
		expect(parseRetryAfter(retryAfter('0'), NOW)).toBe(0);
		expect(parseRetryAfter(retryAfter(httpDateSecondsFromNow(-60)), NOW)).toBe(0);
		expect(parseRetryAfter(new Headers(), NOW)).toBeNull();
		expect(parseRetryAfter(retryAfter('soon'), NOW)).toBeNull();
	});
});

describe('capped waits yield finite countdown text and a terminating timer', () => {
	it.each([
		['delta above the cap', '86400'],
		['400-digit delta', HUGE_DELTA],
		['far-future HTTP-date', FAR_FUTURE_DATE]
	])('%s: capped wait, finite text, terminating timer', (_label, header) => {
		const wait = parseRetryAfter(retryAfter(header), NOW);
		expect(wait).toBe(CAP);

		const text = formatCountdown(wait as number);
		expect(text).toMatch(/^Try again in /);
		expect(text).not.toContain('Infinity');
		expect(text).not.toContain('NaN');

		expect(terminatesAtTryAgainNow(wait)).toBe(true);
	});

	it('zero wait is already the try-again-now state', () => {
		const wait = parseRetryAfter(retryAfter('0'), NOW);
		expect(wait).toBe(0);
		expect(formatCountdown(wait as number)).toBe('You can try again now.');
		expect(terminatesAtTryAgainNow(wait)).toBe(true);
	});
});

describe('toPaywallModel carries the capped wait', () => {
	it.each([
		['400-digit delta', HUGE_DELTA],
		['far-future HTTP-date', FAR_FUTURE_DATE],
		['delta above the cap', '86400']
	])('429 with %s yields the cap and finite countdown text', (_label, header) => {
		vi.setSystemTime(NOW);
		try {
			const model = toPaywallModel({ status: 429, headers: retryAfter(header) });
			expect(model?.kind).toBe('retryable');
			expect(model?.retryAfterSeconds).toBe(CAP);
			expect(model?.countdownText).toBe(formatCountdown(CAP));
			expect(model?.countdownText).not.toContain('Infinity');
			expect(model?.countdownText).not.toContain('NaN');
		} finally {
			vi.useRealTimers();
		}
	});
});
