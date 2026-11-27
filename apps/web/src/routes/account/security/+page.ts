import { redirect } from '@sveltejs/kit';
import type { PageLoad } from './$types';
import { PUBLIC_API_BASE_URL } from '$app/env/public';
import { BillingApiError, fetchSubscription } from '#lib/billing/billingClient.js';
import { captureAuthFence } from '#lib/auth-fence.js';
import { clearSignedInEmail } from '#lib/signed-in-email.js';
import { invalidateLibraryCache } from '#lib/library/cache.js';

// The API session cookie is HttpOnly, so validate it through an authenticated
// read route rather than trying to inspect the cookie or rotate the session.
export const ssr = false;

export const load: PageLoad = async ({ fetch }) => {
	const owner = await captureAuthFence();
	// Keep the session check on browsers without Web Locks. With a
	// fence available, a refused capture may indicate an active transition.
	if (!owner && typeof window !== 'undefined' && globalThis.navigator?.locks) return { sessionCheckFailed: true };
	// One account snapshot, independent of subscription entitlement.
	const baseUrl = PUBLIC_API_BASE_URL.replace(/\/+$/, '');
	const probe = () => fetchSubscription((path, init) => fetch(`${baseUrl}${path}`, {
		...init, headers: { accept: 'application/json' }, cache: 'no-store', redirect: 'error'
	}), owner).then(() => true, (error: unknown) => error);
	const result = owner ? await (await owner.run(() => ({ result: probe() })))?.result : await probe();
	if (result instanceof BillingApiError && result.status === 401) {
		const clear = () => {
			invalidateLibraryCache();
			clearSignedInEmail();
			return true;
		};
		const signedOut = owner ? await owner.run(clear) : clear();
		if (signedOut) throw redirect(307, '/login');
	}
	return { sessionCheckFailed: result !== true || (owner !== undefined && !owner.isCurrent()) };
};
