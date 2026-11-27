import { toLibraryItems } from './cache.js';

export interface RecentPlay {
	key: string;
	trackId: string;
	playedAt: Date;
}

const MAX_PLAYS = 50;
const PLAY_ID = /^[A-Za-z0-9_-]{1,512}$/;
const CATALOGUE_OCCURRENCE = /^([A-Za-z0-9_-]{1,512}):[1-9][0-9]*$/;
const MAX_UNIX_SECONDS = 253_402_300_799; // Last second with a four-digit ISO year.

function isId(value: unknown): value is string {
	return typeof value === 'string' && PLAY_ID.test(value);
}

export async function loadRecentPlays(client: {
	get(path: string): Promise<unknown>;
}): Promise<RecentPlay[]> {
	const payload = await client.get('/v1/me/history');
	const items = (payload as { items?: unknown } | null)?.items;
	if (!Array.isArray(items) || items.length > MAX_PLAYS) throw new Error('Invalid listening history');
	const keys = new Set<string>();
	return items.map((item: unknown) => {
		const { play_id, track_id, played_at } = (item ?? {}) as Record<string, unknown>;
		if (!isId(play_id) || !isId(track_id) || typeof played_at !== 'number' ||
			!Number.isSafeInteger(played_at) || played_at < 0 || played_at > MAX_UNIX_SECONDS) {
			throw new Error('Invalid listening history');
		}
		const key = `${track_id}:${play_id}`;
		if (keys.has(key)) throw new Error('Invalid listening history');
		keys.add(key);
		return { key, trackId: track_id, playedAt: new Date(played_at * 1000) };
	});
}

/** Catalogue rows only label confirmed plays; they never create history entries. */
export function recentPlayTitles(payload: unknown): Map<string, string> {
	const titles = new Map<string, string>();
	const ambiguous = new Set<string>();
	for (const row of toLibraryItems(payload) ?? []) {
		const title = typeof row.title === 'string' ? row.title.trim() : '';
		if (!title) continue;
		// The full job membership is present even when other occurrences fall off the page.
		const recordings = row.recording_mbids;
		const jobId = Array.isArray(recordings) && recordings.length === 1 && recordings[0] === row.id
			&& typeof row.occurrence_key === 'string' ? CATALOGUE_OCCURRENCE.exec(row.occurrence_key)?.[1] : undefined;
		for (const id of [row.id, jobId]) {
			if (!id || ambiguous.has(id)) continue;
			if (titles.has(id) && titles.get(id) !== title) {
				titles.delete(id);
				ambiguous.add(id);
			} else {
				titles.set(id, title);
			}
		}
	}
	return titles;
}
