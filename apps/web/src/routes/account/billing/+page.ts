import type { PageLoad } from './$types';
import { fetchSubscription, fetchUsage } from '$lib/billing/billingClient';

/**
 * Billing page loader. Backend endpoints are implemented by sibling cards;
 * until they land, the page renders with a safe fallback (Plus plan, no
 * usage) instead of failing. No secrets here — session cookie only.
 */
export const load: PageLoad = async ({ fetch }) => {
	try {
		const [subscription, usage] = await Promise.all([
			fetchSubscription(fetch).catch(() => null),
			fetchUsage(fetch).catch(() => null)
		]);
		return {
			planId: subscription?.planId ?? 'plus',
			subscription,
			usage,
			loadError:
				subscription === null && usage === null
					? 'Billing backend is not reachable yet — showing plan defaults.'
					: null
		};
	} catch (err) {
		return {
			planId: 'plus',
			subscription: null,
			usage: null,
			loadError: err instanceof Error ? err.message : 'Could not load billing state.'
		};
	}
};
