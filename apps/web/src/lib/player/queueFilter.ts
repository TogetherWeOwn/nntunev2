/**
 * Shared queue-only presentation matcher: one implementation for the
 * library PlayerQueue panel and the demo queue panel. Filtering hides
 * rows only — it never reorders, reselects, persists or fetches.
 *
 * Contract: trimmed lowercase substring over title/artist, compared after NFC
 * canonical normalization of metadata and query (the stable-id fallback stays
 * byte-literal, case fold only). Blank titles
 * render (and match) as their stable track id, never empty; a missing
 * artist matches as empty, never the string "undefined". The raw query
 * is normalized here, so passing an already-normalized query is a no-op.
 * Pure reads only — the track is never mutated.
 */

import { canonicalMetadata, findNeedle, literalId } from '../library/canonical-compare.js';

export interface QueueFilterTrack {
	id: string;
	title: string;
	artist?: string | null;
}

export function matchesQueueQuery(track: QueueFilterTrack, query: string): boolean {
	const needle = findNeedle(query);
	if (!needle.literal) return true;
	const artist = canonicalMetadata(track.artist ?? '');
	// Title/artist compare canonically; the id fallback compares byte-literally.
	// Queue rows built from untitled library items carry the id as their title,
	// so a title identical to the id is the id fallback and stays byte-literal.
	if (track.title?.trim() && track.title !== track.id) {
		return `${canonicalMetadata(track.title)} ${artist}`.includes(needle.canonical);
	}
	return `${literalId(track.id)} ${artist}`.includes(needle.literal) || artist.includes(needle.canonical);
}
