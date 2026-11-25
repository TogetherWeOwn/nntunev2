import type { LibraryItem } from '../library/cache.js';

const SHELF_LIMIT = 5;

// The caller supplies mirror-enriched catalogue rows in catalogue order, with
// the original ledger owner. The account Library response omits that owner.
export interface DiscoveryCandidate {
	readonly user_id: string | null;
	readonly id: string;
	readonly release_mbid: string | null;
	readonly recording_mbids: readonly string[];
	readonly track_no: number | null;
	readonly title: string | null;
	readonly artist: string | null;
}

export type DiscoveryTrack = Pick<LibraryItem, 'id' | 'title' | 'artist'>;

/** Select one entry per recording from shared, mirror-matched catalogue rows. */
export function selectDiscoveryTracks(candidates: readonly DiscoveryCandidate[]): DiscoveryTrack[] {
	const tracks = new Map<string, DiscoveryTrack>();
	for (const { user_id, id, release_mbid, recording_mbids, track_no, title, artist } of candidates) {
		// A track position is present only for an exact mirror release/recording match.
		if (
			user_id !== null || !release_mbid || track_no === null ||
			!recording_mbids.includes(id) || !title?.trim()
		) continue;
		const recordingId = id.toLowerCase();
		if (tracks.has(recordingId)) continue;
		tracks.set(recordingId, { id: recordingId, title, artist: artist ?? undefined });
		if (tracks.size === SHELF_LIMIT) break;
	}
	return [...tracks.values()];
}
