import type { PageLoad } from './$types';
import { fetchSubscription, fetchUsage } from '$lib/billing/billingClient';
import { resolveEffectivePlan } from '$lib/billing/plans';

/**
 * Billing page loader. Backend endpoints are implemented by sibling cards.
 *
 * The plan is never invented: without an entitlement-granting subscription
 * the page shows trial defaults and says so (ADR 0004). Usage and
 * subscription load independently so one backend failure cannot hide the
 * other. No secrets here — session cookie only.
 */
export const load: PageLoad = async ({ fetch }) => {
	const [subscription, subscriptionError] = await fetchSubscription(fetch).then(
		(s) => [s, null] as const,
		(err) => [null, err instanceof Error ? err.message : 'Could not load subscription.'] as const
	);
	const [usage, usageError] = await fetchUsage(fetch).then(
		(u) => [u, null] as const,
		(err) => [null, err instanceof Error ? err.message : 'Could not load usage.'] as const
	);

	const { plan, planUnknown } = resolveEffectivePlan(subscription);

	const notices: string[] = [];
	if (subscription === null && usage === null) {
		notices.push('Billing backend is not reachable yet — showing trial defaults.');
	} else {
		if (subscriptionError)
			notices.push('Could not load your subscription — showing trial defaults.');
		else if (planUnknown && subscription)
			notices.push(
				`Subscription is ${subscription.status} — showing trial limits until billing is resolved.`
			);
		if (usageError) notices.push('Could not load usage.');
	}

	return {
		planId: plan.id,
		planUnknown,
		subscription,
		usage,
		loadError: notices.length > 0 ? notices.join(' ') : null
	};
};
