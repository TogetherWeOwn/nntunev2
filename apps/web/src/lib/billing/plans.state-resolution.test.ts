/**
 * Acceptance contract: display-only plan resolution confirms a plan only
 * on the server-derived `state: 'active'` (spec §4 / ADR 0005) — never on
 * a stored status the current model does not entitle. There is no trial
 * product state anymore: a stored `trialing` is unknown to the model and
 * must fail closed exactly like an expired trial used to (this file was
 * the trial-deadline contract before the trial was removed).
 *
 * The implementation under contract is `resolveEffectivePlan` in
 * `./plans.ts`:
 *
 * - `state: 'active'` on a known paid tier confirms; the server already
 *   folded settlement and grace into that state, so the stored status
 *   (e.g. in-grace `past_due`) cannot veto it client-side.
 * - `state: 'lapsed'` / `'no_subscription'` never confirm, whatever the
 *   stored status says — a stale or mismatched client payload must not
 *   outrank the server fold.
 * - `active` on the reserved free plan is never confirmed: free is a
 *   0-hour reserved row, not an entitled tier.
 * - Stateless legacy refs (no `state`) keep their behavior: `active`
 *   paid tiers confirm, `past_due` confirms only strictly inside its
 *   grace window (a pure function of the injected clock), and every
 *   other stored status — `trialing` included — fails closed to the
 *   reserved row with `planUnknown: true`.
 *
 * This is display resolution only. Nothing here licenses client-side
 * access enforcement; the server remains authoritative.
 */
import { describe, expect, it } from 'vitest';
import { getPlan, resolveEffectivePlan, type SubscriptionRef } from './plans';

const GRACE_END = '2026-10-29T12:00:00.000Z';
const BEFORE = new Date('2026-10-29T11:59:59.999Z');
const AT = new Date('2026-10-29T12:00:00.000Z');
const AFTER = new Date('2026-10-29T12:00:00.001Z');

describe('server state is authoritative when present', () => {
	it('confirms a paid tier on state active, whatever the stored status', () => {
		for (const status of ['active', 'past_due', 'trialing'] as const) {
			const r = resolveEffectivePlan({ planId: 'pro', status, state: 'active' }, AFTER);
			expect(r.plan.id, `status=${status}`).toBe('pro');
			expect(r.planUnknown, `status=${status}`).toBe(false);
		}
	});

	it.each([
		['lapsed', 'active'],
		['lapsed', 'trialing'],
		['no_subscription', null]
	] as const)(
		'does not confirm on state %s even with stored status %s',
		(state, status) => {
			const r = resolveEffectivePlan(
				{ planId: status === null ? null : 'pro', status, state },
				BEFORE
			);
			expect(r.planUnknown).toBe(true);
			expect(r.plan.id).toBe('free');
		}
	);

	it('still refuses an unknown plan id on state active', () => {
		const r = resolveEffectivePlan(
			{ planId: 'legacy-2019', status: 'active', state: 'active' },
			BEFORE
		);
		expect(r.planUnknown).toBe(true);
		expect(r.plan.id).toBe('free');
	});
});

describe('active free is not an entitled tier', () => {
	it.each([
		['with server state', { planId: 'free', status: 'active', state: 'active' }],
		['stateless', { planId: 'free', status: 'active' }]
	] as const)('does not confirm active free %s', (_label, sub: SubscriptionRef) => {
		const r = resolveEffectivePlan(sub, BEFORE);
		expect(r.planUnknown).toBe(true);
		expect(r.plan.id).toBe('free');
	});
});

describe('stateless refs: stored trialing fails closed like an expired trial', () => {
	it.each([BEFORE, AT, AFTER, new Date('2027-10-29T12:00:00.000Z')])(
		'never confirms trialing, at any injected instant (%s)',
		(now) => {
			for (const planId of ['free', 'pro'] as const) {
				const r = resolveEffectivePlan({ planId, status: 'trialing' }, now);
				expect(r.planUnknown, `planId=${planId}`).toBe(true);
				expect(r.plan.id, `planId=${planId}`).toBe('free');
			}
		}
	);

	it('ignores stray legacy trial fields on the payload', () => {
		const sub = {
			planId: 'pro',
			status: 'trialing',
			trialEndsAt: '2099-01-01T00:00:00.000Z'
		} as SubscriptionRef;
		const r = resolveEffectivePlan(sub, BEFORE);
		expect(r.planUnknown).toBe(true);
		expect(r.plan.id).toBe('free');
	});
});

describe('stateless refs: grace window is a pure function of the injected clock', () => {
	const sub: SubscriptionRef = { planId: 'pro', status: 'past_due', graceUntil: GRACE_END };

	it('confirms one millisecond before the window closes, not at/after it', () => {
		expect(resolveEffectivePlan(sub, BEFORE).planUnknown).toBe(false);
		expect(resolveEffectivePlan(sub, AT).planUnknown).toBe(true);
		expect(resolveEffectivePlan(sub, AFTER).planUnknown).toBe(true);
		// Deterministic: repeating a call cannot flip the verdict.
		expect(resolveEffectivePlan(sub, BEFORE)).toEqual(resolveEffectivePlan(sub, BEFORE));
		expect(resolveEffectivePlan(sub, AT)).toEqual(resolveEffectivePlan(sub, AT));
	});

	it.each([
		['omitted', undefined],
		['null', null],
		['empty string', ''],
		['unparseable', 'not-a-date']
	])('does not confirm past_due whose grace window is %s', (_label, graceUntil) => {
		const r = resolveEffectivePlan({ planId: 'pro', status: 'past_due', graceUntil }, BEFORE);
		expect(r.planUnknown).toBe(true);
		expect(r.plan.id).toBe('free');
	});
});

describe('controls: confirmed states keep their existing display behavior', () => {
	it('still confirms an active paid tier (stateless)', () => {
		const r = resolveEffectivePlan({ planId: 'plus', status: 'active' }, AFTER);
		expect(r.plan.id).toBe('plus');
		expect(r.planUnknown).toBe(false);
	});

	it('still confirms the top paid tier while active', () => {
		const r = resolveEffectivePlan({ planId: 'max', status: 'active', state: 'active' }, AFTER);
		expect(r.plan).toEqual(getPlan('max'));
		expect(r.planUnknown).toBe(false);
	});

	it.each(['canceled', 'expired', 'incomplete'] as const)(
		'still does not confirm status %s',
		(status) => {
			const r = resolveEffectivePlan({ planId: 'pro', status }, BEFORE);
			expect(r.planUnknown).toBe(true);
			expect(r.plan.id).toBe('free');
		}
	);

	it('still falls back unconfirmed when there is no subscription', () => {
		const r = resolveEffectivePlan(null, BEFORE);
		expect(r.plan.id).toBe('free');
		expect(r.planUnknown).toBe(true);
	});
});
