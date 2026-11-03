/**
 * Plan matrix for the NNTune billing UI (ADR 0004 as revised 2026-11-03:
 * Plus / Pro / Max, no trial, no free tier).
 *
 * The `plans` table is the one runtime source for prices and caps; the
 * signed-in pricing page hydrates from `GET /v1/plans` and merges those
 * numbers over the names/blurbs here (`planFromDTO`). This module is the
 * frontend fallback + display-metadata mirror for signed-out visitors —
 * the same parity-mirror role the backend `PlanId` enum plays. Prices are
 * proposals until the owner confirms them for live billing (spec §2.1).
 *
 * Security: this module is display-only. Enforcement happens server-side
 * (the API worker reads `subscriptions` + `plans` + the stream-play
 * ledger fold). Never gate access on these client values.
 */

export interface Plan {
	id: string;
	name: string;
	priceCents: number;
	streamHoursMonth: number;
	maxConcurrentStreams: number;
	/** False only for the reserved `free` row: shown nowhere, chosen never. */
	selectable: boolean;
	blurb: string;
}

/**
 * Max blurb from the live cap, never a second hard-coded figure: the
 * static row and `planFromDTO` share this, so a plans-table change can
 * never leave the blurb quoting hours the quota cell contradicts. The
 * "unlimited" wording (spec §2.1) is allowed only with the figure
 * present, and only while the figure is a physical month ceiling
 * (31 days × 24 h = 744) — below that the blurb states the hours plainly.
 */
function maxBlurb(streamHoursMonth: number): string {
	return streamHoursMonth >= 744
		? `Unlimited listening — ${streamHoursMonth} hours a month, every hour there is.`
		: `${streamHoursMonth} hours a month of listening.`;
}

export const PLANS: Plan[] = [
	{
		id: 'free',
		name: 'No plan',
		priceCents: 0,
		streamHoursMonth: 0,
		maxConcurrentStreams: 1,
		selectable: false,
		blurb: 'Reserved row — browsing only, no playback.'
	},
	{
		id: 'plus',
		name: 'Plus',
		priceCents: 800,
		streamHoursMonth: 100,
		maxConcurrentStreams: 1,
		selectable: true,
		blurb: 'Everyday listening — about 3 hours a day.'
	},
	{
		id: 'pro',
		name: 'Pro',
		priceCents: 1500,
		streamHoursMonth: 300,
		maxConcurrentStreams: 1,
		selectable: true,
		blurb: 'Long days of listening — about 10 hours a day.'
	},
	{
		id: 'max',
		name: 'Max',
		priceCents: 2500,
		streamHoursMonth: 750,
		maxConcurrentStreams: 1,
		selectable: true,
		// `maxBlurb` keeps the "unlimited" wording (spec §2.1) next to the
		// live figure; the pricing table's hours column shows it too.
		blurb: maxBlurb(750)
	}
];

export function getPlan(id: string): Plan | undefined {
	return PLANS.find((p) => p.id === id);
}

export function listPlans(): Plan[] {
	return [...PLANS];
}

/** Plans a visitor may actually choose (the reserved `free` row never). */
export function listSelectablePlans(): Plan[] {
	return PLANS.filter((p) => p.selectable);
}

/**
 * Merge one `GET /v1/plans` row over the static display metadata: prices
 * and caps come from plan data (never a second hard-coded copy in the
 * page), names/blurbs stay frontend copy. Unknown ids return undefined —
 * the client cannot name a plan it has no copy for, so it never renders
 * or offers it.
 */
export function planFromDTO(dto: {
	id: string;
	priceCents: number;
	streamHoursMonth: number;
	maxConcurrentStreams: number;
	selectable: boolean;
}): Plan | undefined {
	const base = getPlan(dto.id);
	if (!base) return undefined;
	return {
		...base,
		priceCents: dto.priceCents,
		streamHoursMonth: dto.streamHoursMonth,
		maxConcurrentStreams: dto.maxConcurrentStreams,
		selectable: dto.selectable,
		// Quota-dependent copy regenerates from the hydrated cap so the
		// blurb can never quote a superseded figure next to the new quota.
		blurb: dto.id === 'max' ? maxBlurb(dto.streamHoursMonth) : base.blurb
	};
}

export function formatPrice(plan: Plan): string {
	if (plan.priceCents === 0) return '$0';
	return `$${plan.priceCents / 100}`;
}

export function formatPricePerMonth(plan: Plan): string {
	if (plan.priceCents === 0) return '$0';
	return `$${plan.priceCents / 100}/mo`;
}

/**
 * Exposed accessible name for a paid plan's pricing-table button. The name
 * always carries the plan, so assistive tech can tell the tiers apart even
 * though the visible label stays a shared "Choose" / "Starting…".
 */
export function planButtonName(plan: Plan, pendingPlanId: string | null): string {
	return pendingPlanId === plan.id ? `Starting ${plan.name}…` : `Choose ${plan.name}`;
}

/** Stream cap in seconds (plans table stores hours). */
export function streamCapSeconds(plan: Plan): number {
	return plan.streamHoursMonth * 3600;
}

/**
 * Minimal subscription shape for plan resolution. Kept structural (instead
 * of importing SubscriptionDTO) so this module stays dependency-free.
 * `state` is the server-derived account state (spec §4); when present it
 * is authoritative for display — the client never re-derives entitlement
 * windows the server already resolved.
 */
export interface SubscriptionRef {
	planId: string | null;
	status: string | null;
	state?: 'no_subscription' | 'active' | 'lapsed';
	/** End of the past-due grace window (ADR 0002 rule 4); null/omitted when none. */
	graceUntil?: string | null;
}

export interface EffectivePlan {
	plan: Plan;
	/**
	 * True when the subscription is missing, unknown, or not entitlement-
	 * granting: the caller is seeing reserved defaults, not the user's
	 * real plan, and must say so instead of naming a paid tier.
	 */
	planUnknown: boolean;
}

/**
 * True while a past-due subscription is still inside its grace window
 * (ADR 0002 rule 4): the server keeps enforcing the real plan's caps, so
 * the UI must score usage against them too. An unparseable or missing
 * window fails closed to lapsed.
 */
function inGraceWindow(subscription: SubscriptionRef, now: Date): boolean {
	if (subscription.status !== 'past_due') return false;
	if (!subscription.graceUntil) return false;
	return new Date(subscription.graceUntil).getTime() > now.getTime();
}

/**
 * Display plan: the server-derived `active` state on a known paid tier,
 * or (for payloads without a state) an `active` paid subscription /
 * `past_due` inside its grace window; the reserved 0-hour row with
 * `planUnknown` otherwise. There is no trial state to confirm — a stored
 * `trialing` status is unknown to the current model and fails closed
 * like every other non-entitling status. This does not enforce access:
 * the server remains authoritative.
 */
export function resolveEffectivePlan(
	subscription: SubscriptionRef | null,
	now: Date = new Date()
): EffectivePlan {
	const plan = subscription?.planId ? getPlan(subscription.planId) : undefined;
	if (
		subscription &&
		plan &&
		plan.id !== 'free' &&
		(subscription.state === 'active' ||
			(subscription.state === undefined &&
				(subscription.status === 'active' || inGraceWindow(subscription, now))))
	) {
		return { plan, planUnknown: false };
	}
	return { plan: getPlan('free')!, planUnknown: true };
}

/** Next upgrade step for prompts, or null when already at the top. */
export function nextPlan(id: string): Plan | undefined {
	const order = ['free', 'plus', 'pro', 'max'];
	const idx = order.indexOf(id);
	if (idx < 0 || idx >= order.length - 1) return undefined;
	return getPlan(order[idx + 1]);
}
