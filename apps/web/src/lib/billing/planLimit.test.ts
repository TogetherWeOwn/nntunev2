/**
 * Failing-first acceptance tests for plan-limit 402/429 handling with upgrade CTA.
 *
 * This acceptance test is frozen; change it only through its owning work item.
 *
 * Contract implemented in the sibling source module
 * `src/lib/billing/planLimit.ts` (imported below as `./planLimit.js`, matching
 * this codebase's `.js`-suffix lib imports):
 *
 * - `toPaywallModel(res: { status: number; headers: Headers; body?: unknown })`
 *   returns a `PaywallModel | null` — non-null for 402/429 limit responses,
 *   null for anything else (no dead-end error toast: a limit always yields a
 *   paywall model, never null).
 * - `PaywallModel` shape:
 *   `{ kind: 'terminal' | 'retryable'; ctaHref: '/pricing' | null;
 *      headline: string; detail: string;
 *      retryAfterSeconds: number | null; countdownText: string | null }`
 *   - 402 -> `kind: 'terminal'`, `retryAfterSeconds: null`, `countdownText: null`.
 *   - 429 -> `kind: 'retryable'`, `retryAfterSeconds` parsed from the
 *     `Retry-After` response header (delta-seconds), `countdownText` names
 *     that many seconds (e.g. contains "45" for `Retry-After: 45`).
 *   - `ctaHref` is `/pricing` wherever an upgrade changes the answer;
 *     `headline`/`detail` are non-empty and name the current usage. The
 *     concurrent-stream lease 429 (`x-plan-limit: concurrent_streams=...`)
 *     is the exception: no plan raises it, so `ctaHref` is null and the
 *     copy names the other device instead.
 * - `PAYWALL_SHOWN_EVENT` is the documented analytics event name:
 *   `'usage_limit_paywall_shown'`.
 * - `trackPaywallShown(track, model)` calls `track(name, props)` exactly once
 *   with that event name; props carry only coarse limit facts
 *   (`kind`, `status`, `code`, `retryAfterSeconds`) — never PII
 *   (no email, user id, token, or password).
 *
 * These tests dynamic-import the module so they stay statically valid
 * (svelte-check clean) while failing at runtime until the builder lands the
 * implementation. Each failure below currently throws the handoff error from
 * `loadPlanLimit()`; once the module exists, the same assertions verify behavior.
 */

import { describe, expect, it } from 'vitest';

const NOT_IMPLEMENTED =
	'planLimit module not implemented yet (this acceptance test is intentionally failing-first)';

// eslint-disable-next-line @typescript-eslint/no-explicit-any
async function loadPlanLimit(): Promise<any> {
	// Non-literal specifier on purpose: static typecheck stays green while the
	// module is missing (failing-first), and the runtime import throws until
	// the implementation lands planLimit.ts next to this file.
	const specifier = './planLimit.js';
	try {
		return await import(specifier);
	} catch {
		throw new Error(NOT_IMPLEMENTED);
	}
}

function limitResponse(status: number, retryAfter: string | null, code: string): Response {
	const headers = new Headers();
	if (retryAfter !== null) headers.set('Retry-After', retryAfter);
	return new Response(JSON.stringify({ code, message: 'limit reached' }), {
		status,
		headers
	});
}

describe('plan-limit 402/429 handling with upgrade CTA', () => {
	it('mocked 402 renders a paywall with an upgrade CTA to /pricing', async () => {
		const planLimit = await loadPlanLimit();
		const res = limitResponse(402, null, 'plan_limit_exceeded');

		const model = planLimit.toPaywallModel({ status: res.status, headers: res.headers });

		expect(model).not.toBeNull();
		expect(model.kind).toBe('terminal');
		expect(model.ctaHref).toBe('/pricing');
		expect(model.headline.length).toBeGreaterThan(0);
		expect(model.detail.length).toBeGreaterThan(0);
		expect(model.retryAfterSeconds).toBeNull();
		expect(model.countdownText).toBeNull();
	});

	it('mocked 429 renders a retry countdown from Retry-After', async () => {
		const planLimit = await loadPlanLimit();
		const res = limitResponse(429, '45', 'rate_limited');

		const model = planLimit.toPaywallModel({ status: res.status, headers: res.headers });

		expect(model).not.toBeNull();
		expect(model.kind).toBe('retryable');
		expect(model.ctaHref).toBe('/pricing');
		expect(model.retryAfterSeconds).toBe(45);
		expect(model.countdownText).toContain('45');
	});

	it('402 subscription_required renders subscribe copy, never limit copy', async () => {
		const planLimit = await loadPlanLimit();

		const model = planLimit.toPaywallModel({
			status: 402,
			headers: new Headers(),
			body: { code: 'subscription_required', reason: 'never_subscribed' }
		});

		expect(model).not.toBeNull();
		expect(model.kind).toBe('terminal');
		expect(model.code).toBe('subscription_required');
		expect(model.ctaHref).toBe('/pricing');
		expect(model.headline.toLowerCase()).toContain('subscribe');
		// No limit was reached: the copy must not claim one, and there is
		// no countdown on a terminal refusal.
		expect(model.detail.toLowerCase()).not.toContain('limit');
		expect(model.detail.toLowerCase()).not.toContain('usage');
		expect(model.retryAfterSeconds).toBeNull();
		expect(model.countdownText).toBeNull();
	});

	it('402 subscription_required with reason lapsed renders reactivate copy', async () => {
		const planLimit = await loadPlanLimit();

		const model = planLimit.toPaywallModel({
			status: 402,
			headers: new Headers(),
			body: { code: 'subscription_required', reason: 'lapsed' }
		});

		expect(model).not.toBeNull();
		expect(model.kind).toBe('terminal');
		expect(model.ctaHref).toBe('/account/billing');
		expect(model.headline.toLowerCase()).toContain('lapsed');
		expect(model.detail.toLowerCase()).toContain('active again');
		expect(model.detail.toLowerCase()).not.toContain('limit');
	});

	it('402 subscription_required without a known reason falls back to subscribe copy', async () => {
		const planLimit = await loadPlanLimit();

		for (const body of [
			{ code: 'subscription_required' },
			{ code: 'subscription_required', reason: 'something-else' },
			{ code: 'subscription_required', reason: 42 }
		]) {
			const model = planLimit.toPaywallModel({ status: 402, headers: new Headers(), body });
			expect(model).not.toBeNull();
			expect(model.headline.toLowerCase()).toContain('subscribe');
			expect(model.detail.toLowerCase()).toContain('needs a plan');
			expect(model.detail.toLowerCase()).not.toContain('resubscribe');
		}
	});

	it('429 concurrent-stream lease names the other device and offers no upgrade', async () => {
		const planLimit = await loadPlanLimit();
		const headers = new Headers();
		headers.set('Retry-After', '30');
		headers.set('x-plan-limit', 'concurrent_streams=1');

		const model = planLimit.toPaywallModel({
			status: 429,
			headers,
			body: { code: 'rate_limited', message: 'another session is playing' }
		});

		expect(model).not.toBeNull();
		expect(model.kind).toBe('retryable');
		// No plan raises the concurrent allowance: an upgrade CTA here
		// would send the user to pricing and change nothing.
		expect(model.ctaHref).toBeNull();
		expect(model.headline.toLowerCase()).toContain('another device');
		expect(model.detail.toLowerCase()).toContain('other device');
		expect(model.detail.toLowerCase()).not.toContain('upgrade');
		expect(model.retryAfterSeconds).toBe(30);
		expect(model.countdownText).toContain('30');
	});

	it('429 without the concurrent dimension keeps the upgrade CTA', async () => {
		const planLimit = await loadPlanLimit();
		const headers = new Headers();
		headers.set('Retry-After', '30');
		headers.set('x-plan-limit', 'stream_hours_month=100');

		const model = planLimit.toPaywallModel({
			status: 429,
			headers,
			body: { code: 'rate_limited' }
		});

		expect(model).not.toBeNull();
		expect(model.ctaHref).toBe('/pricing');
		expect(model.headline.toLowerCase()).not.toContain('another device');
	});

	it('paywall display fires the documented analytics event with no PII', async () => {
		const planLimit = await loadPlanLimit();
		expect(planLimit.PAYWALL_SHOWN_EVENT).toBe('usage_limit_paywall_shown');

		const res = limitResponse(402, null, 'plan_limit_exceeded');
		const model = planLimit.toPaywallModel({ status: res.status, headers: res.headers });
		expect(model).not.toBeNull();

		const calls: Array<{ name: string; props: Record<string, unknown> }> = [];
		const track = (name: string, props: Record<string, unknown>): void => {
			calls.push({ name, props });
		};
		planLimit.trackPaywallShown(track, model);

		expect(calls).toHaveLength(1);
		expect(calls[0].name).toBe('usage_limit_paywall_shown');
		const serialized = JSON.stringify(calls[0].props).toLowerCase();
		for (const forbidden of ['email', 'userid', 'user_id', 'token', 'password', '@']) {
			expect(serialized).not.toContain(forbidden);
		}
	});
});
