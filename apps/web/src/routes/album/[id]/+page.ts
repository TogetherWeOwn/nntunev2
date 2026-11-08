import type { PageLoad } from './$types';
import { PUBLIC_API_BASE_URL } from '$app/env/public';
import { ApiClient } from '#lib/api/client.js';
import { readArtist, readRelease } from '#lib/catalogue/catalogue.js';

export const ssr = false;

export const load: PageLoad = async ({ fetch, params, depends }) => {
	depends('catalogue:album');
	const client = new ApiClient({ baseUrl: PUBLIC_API_BASE_URL, fetchFn: fetch });
	const result = await readRelease(client, params.id);
	const artistId = result.value?.release.artist_mbid;
	const artist = artistId ? await readArtist(client, artistId) : null;
	return { ...result, artist: artist?.value ?? null };
};
