import { redirect } from '@sveltejs/kit';
import type { PageLoad } from './$types';
import { PUBLIC_API_BASE_URL } from '$app/env/public';
import { ApiClient, ApiError } from '#lib/api/client.js';
import { captureAuthFence } from '#lib/auth-fence.js';
import { clearSignedInEmail } from '#lib/signed-in-email.js';
import { invalidateLibraryCache } from '#lib/library/cache.js';

// The API session cookie is HttpOnly, so validate it through an authenticated
// read route rather than trying to inspect the cookie or rotate the session.
export const ssr = false;

export const load: PageLoad = async ({ fetch }) => {
	const owner = await captureAuthFence();
	if (!owner) return { sessionCheckFailed: true };
	const client = new ApiClient({ baseUrl: PUBLIC_API_BASE_URL, fetchFn: fetch, session: { owner } });
	// One account snapshot, independent of subscription entitlement.
	const sent = await owner.run(() => ({
		result: client.get<unknown>('/v1/me/subscription').then(() => true, (error: unknown) => error)
	}));
	const result = await sent?.result;
	if (result instanceof ApiError && result.status === 401) {
		const signedOut = await owner.run(() => {
			invalidateLibraryCache();
			clearSignedInEmail();
			return true;
		});
		if (signedOut) throw redirect(307, '/login');
	}
	return { sessionCheckFailed: result !== true || !owner.isCurrent() };
};
