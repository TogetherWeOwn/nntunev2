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
 *   `{ kind: 'terminal' | 'retryable'; ctaHref: '/pricing';
 *      headline: string; detail: string;
 *      retryAfterSeconds: number | null; countdownText: string | null }`
 *   - 402 -> `kind: 'terminal'`, `retryAfterSeconds: null`, `countdownText: null`.
 *   - 429 -> `kind: 'retryable'`, `retryAfterSeconds` parsed from the
 *     `Retry-After` response header (delta-seconds), `countdownText` names
 *     that many seconds (e.g. contains "45" for `Retry-After: 45`).
 *   - `ctaHref` is always `/pricing`; `headline`/`detail` are non-empty and
 *     name the current usage.
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
