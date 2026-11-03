import type { PageLoad } from './$types';
import {
	BillingApiError,
	fetchSubscription,
	fetchUsage,
	type SubscriptionDTO
} from '#lib/billing/billingClient.js';
import { getPlan, resolveEffectivePlan } from '#lib/billing/plans.js';
import { evaluateUsage, type LimitStatus, type UsageSnapshot } from '#lib/billing/usage.js';

export type UsageDTO = UsageSnapshot & { period: string; resetsAt: string };

/**
 * Stored subscription states without entitlement (ADR 0002 data-model,
 * ADR 0005 billing table): portal, never checkout. `unpaid`/`paused` keep
 * the reserved slot and `incomplete_expired` has released it, but all
 * three still owe the portal — never a second subscription. `trialing`
 * joins them: no trial product state exists anymore, but a legacy stored
 * row in that status still names a Stripe customer, so the portal is its
 * route too.
 */
const LAPSED = new Set([
	'past_due',
	'canceled',
	'unpaid',
	'paused',
	'incomplete',
	'incomplete_expired',
	'trialing'
]);

interface LoadOutcome<T> {
	value: T | null;
	/** HTTP status when the call failed with one; null for network failures. */
	status: number | null;
	failed: boolean;
}

async function loadOne<T>(read: () => Promise<T>): Promise<LoadOutcome<T>> {
	try {
		return { value: await read(), status: null, failed: false };
	} catch (err) {
		return {
			value: null,
			status: err instanceof BillingApiError ? err.status : null,
			failed: true
		};
	}
}

/**
 * Billing page loader over the api worker's billing/usage endpoints.
 *
 * The plan is never invented: without an entitlement-granting subscription
 * the page shows no-plan defaults and says so. The server-derived `state`
 * (spec §4) drives the surfaces: `no_subscription` gets the subscribe
 * notice (browsing and library stay usable — only playback is gated),
 * `lapsed` resolves to the billing portal, never to a new checkout, so a
 * paying user can neither be downgraded nor double-subscribed. Usage is
 * scored against caps only when the plan is confirmed — otherwise no usage
 * alert or upgrade button is rendered, because the caps being compared
 * against would be the wrong plan's. A 401 is reported as signed-out, not
 * as an outage. No secrets here — session cookie only.
 *
 * Client-rendered on purpose (`ssr = false`): this page is session-gated,
 * so SSR buys nothing, and a server-side `fetch('/v1/...')` never leaves
 * the web worker (SvelteKit internal fetch) — it 404s against the app's
 * own routes instead of reaching the API worker. In the browser the same
 * relative URLs go over the network through the edge route table.
 */
export const ssr = false;

export const load: PageLoad = async ({ fetch }) => {
	// Independent reads share one deadline window; loadOne keeps failures isolated.
	const [sub, use] = await Promise.all([
		loadOne(() => fetchSubscription(fetch)),
		loadOne(() => fetchUsage(fetch))
	]);

	// The subscription endpoint answers 200 with a server-derived state
	// for every session — a missing record is `no_subscription`, not a
	// 404 — so any subscription failure is a backend failure and surfaces
	// the notice with a Retry. A 404 on usage stays non-fatal: a fresh
	// account may genuinely have nothing metered yet, and the meters
	// simply don't render.
	const subscription: SubscriptionDTO | null = sub.value;
	const usage: UsageDTO | null = use.status === 404 ? null : use.value;
	// Limit statuses are already surfaced by the global paywall listener; do
	// not stack a generic billing-outage notice beside it. A 429 still needs
	// the page Retry control once its countdown allows another attempt.
	const isLimitStatus = (status: number | null) => status === 402 || status === 429;
	const retryableLimit = sub.status === 429 || use.status === 429;
	const subFailed = sub.failed && !isLimitStatus(sub.status);
	const usageFailed = use.failed && use.status !== 404 && !isLimitStatus(use.status);
	const signedOut = sub.status === 401 || use.status === 401;

	const { plan, planUnknown } = resolveEffectivePlan(subscription);
	const manageBillingOnly =
		subscription !== null &&
		(subscription.state === 'lapsed' ||
			(subscription.status !== null && LAPSED.has(subscription.status)));
	let usageStatus: LimitStatus | null =
		!planUnknown && usage ? evaluateUsage(plan, usage) : null;
	// Portal-only: clearing the upgrade target keeps usage scored for display
	// while the page renders Manage billing as the sole action. A lapsed
	// subscriber must never start a new checkout — that would double-subscribe
	// them next to the invoice they already owe.
	if (manageBillingOnly && usageStatus) usageStatus = { ...usageStatus, upgradeTo: null };

	const notices: string[] = [];
	if (signedOut) {
		notices.push('You are signed out — sign in to see your billing.');
	} else if (subFailed && usageFailed) {
		notices.push('Billing backend is not reachable yet — showing defaults.');
	} else {
		if (subFailed)
			notices.push('Could not load your subscription — showing defaults.');
		else if (planUnknown && subscription) {
			notices.push(
				subscription.state === 'no_subscription'
					? "You don't have a subscription yet — see plans to start listening. Browsing and your library are fully usable."
					: `Subscription is ${subscription.status ?? 'lapsed'} — manage billing to resolve; playback resumes once your subscription is active again.`
			);
		} else if (manageBillingOnly && subscription)
			// Reachable for confirmed `past_due` (every other lapsed state is
			// planUnknown): the plan is confirmed so no other notice renders,
			// yet the failed payment still needs attention. `past_due` covers
			// two server states, and only one is a grace period — with
			// `graceUntil` set, access runs on renewed grace; without it the
			// subscription is still inside already-paid coverage and no grace
			// window has started, so the copy must not name one.
			notices.push(
				subscription.graceUntil
					? 'Your last payment failed — manage billing to resolve; your plan stays active during the grace period.'
					: 'Your last payment failed — manage billing to resolve; your plan stays active through the end of your paid period.'
			);
		if (usageFailed) notices.push('Could not load usage.');
	}

	// Keep a known stored billing tier for portal-only states without granting
	// entitlement or scoring usage against that tier.
	const billingPlan =
		manageBillingOnly && subscription?.planId ? getPlan(subscription.planId) : undefined;
	return {
		planId: billingPlan?.id ?? plan.id,
		planUnknown,
		signedOut,
		hadFetchError: !signedOut && (subFailed || usageFailed || retryableLimit),
		manageBillingOnly,
		subscription,
		usage,
		usageStatus,
		loadError: notices.length > 0 ? notices.join(' ') : null
	};
};
