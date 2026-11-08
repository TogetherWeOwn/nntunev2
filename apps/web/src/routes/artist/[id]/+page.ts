import type { PageLoad } from './$types';
import { PUBLIC_API_BASE_URL } from '$app/env/public';
import { ApiClient } from '#lib/api/client.js';
import { readArtist } from '#lib/catalogue/catalogue.js';

export const ssr = false;

export const load: PageLoad = async ({ fetch, params, depends }) => {
	depends('catalogue:artist');
	return readArtist(new ApiClient({ baseUrl: PUBLIC_API_BASE_URL, fetchFn: fetch }), params.id);
};
