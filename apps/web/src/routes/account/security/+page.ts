import { redirect } from '@sveltejs/kit';
import type { PageLoad } from './$types';
import { PUBLIC_API_BASE_URL } from '$app/env/public';
import { ApiClient, ApiError } from '#lib/api/client.js';
import { clearSignedInEmail } from '#lib/signed-in-email.js';

// The API session cookie is HttpOnly, so validate it through an authenticated
// read route rather than trying to inspect the cookie or rotate the session.
export const ssr = false;

export const load: PageLoad = async ({ fetch }) => {
	const client = new ApiClient({ baseUrl: PUBLIC_API_BASE_URL, fetchFn: fetch });
	let sessionCheckFailed = false;
	try {
		await client.get<unknown>('/v1/vault');
	} catch (error) {
		if (error instanceof ApiError && error.status === 401) {
			clearSignedInEmail();
			throw redirect(307, '/login');
		}
		sessionCheckFailed = true;
	}

	return { sessionCheckFailed };
};
