import { toPublicDiscoveryTrack, type DiscoveryCandidate } from './discovery-shelf.js';

const SHELF_LIMIT = 5;

/** One decoded artist-to-recording relationship with normalized MBIDs. */
export interface ArtistRecordingRelationship {
	readonly artistMbid: string;
	readonly recordingMbid: string;
	readonly linkType: string;
}

export interface RelatedArtist {
	readonly id: string;
	readonly name: string;
}

/** Rank known mirror artists by the number of distinct public recordings shared with the seed. */
export function selectRelatedArtists(
	seedArtistMbid: string,
	catalogue: readonly DiscoveryCandidate[],
	mirrorArtistNames: ReadonlyMap<string, string>,
	relationships: readonly ArtistRecordingRelationship[]
): RelatedArtist[] {
	const seed = seedArtistMbid.toLowerCase();
	if (!mirrorArtistNames.get(seed)?.trim()) return [];

	const publicRecordings = new Set<string>();
	for (const candidate of catalogue) {
		const track = toPublicDiscoveryTrack(candidate);
		if (track) publicRecordings.add(track.id);
	}

	const seedRecordings = new Set<string>();
	for (const { artistMbid, recordingMbid } of relationships) {
		if (artistMbid === seed && publicRecordings.has(recordingMbid)) seedRecordings.add(recordingMbid);
	}
	if (seedRecordings.size === 0) return [];

	const matches = new Map<string, { name: string; recordings: Set<string> }>();
	for (const { artistMbid, recordingMbid } of relationships) {
		if (artistMbid === seed || !seedRecordings.has(recordingMbid)) continue;
		const name = mirrorArtistNames.get(artistMbid)?.trim();
		if (!name) continue;
		let match = matches.get(artistMbid);
		if (!match) {
			match = { name, recordings: new Set() };
			matches.set(artistMbid, match);
		}
		match.recordings.add(recordingMbid);
	}

	return [...matches]
		.sort(([a, left], [b, right]) => right.recordings.size - left.recordings.size || (a < b ? -1 : a > b ? 1 : 0))
		.slice(0, SHELF_LIMIT)
		.map(([id, { name }]) => ({ id, name }));
}
