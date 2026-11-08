import type { PageLoad } from './$types';
import { PUBLIC_API_BASE_URL } from '$app/env/public';
import { ApiClient } from '#lib/api/client.js';
import { catalogueAuthGeneration } from '#lib/catalogue/auth.svelte.js';
import { readOccurrence } from '#lib/catalogue/occurrence.js';

export const ssr = false;

export const load: PageLoad = async ({ fetch, params, url, depends }) => {
	depends('catalogue:track');
	const client = new ApiClient({ baseUrl: PUBLIC_API_BASE_URL, fetchFn: fetch });
	return readOccurrence(client, params.id, url.searchParams, catalogueAuthGeneration);
};
