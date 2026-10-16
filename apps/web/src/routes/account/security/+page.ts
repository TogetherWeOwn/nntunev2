import { redirect } from '@sveltejs/kit';
import type { PageLoad } from './$types';
import { PUBLIC_API_BASE_URL } from '$app/env/public';
import { ApiClient, ApiError } from '#lib/api/client.js';

// The API session cookie is HttpOnly, so validate it through the API's
// authenticated refresh route rather than trying to inspect the cookie.
export const ssr = false;

export const load: PageLoad = async ({ fetch }) => {
	const client = new ApiClient({ baseUrl: PUBLIC_API_BASE_URL, fetchFn: fetch });
	try {
		await client.post<{ ok: true }>('/v1/session/refresh', {});
	} catch (error) {
		if (error instanceof ApiError && error.status === 401) {
			throw redirect(307, '/login');
		}
	}

	return {};
};
