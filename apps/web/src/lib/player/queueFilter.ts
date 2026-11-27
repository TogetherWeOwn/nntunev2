/**
 * Shared queue-only presentation matcher: one implementation for the
 * library PlayerQueue panel and the demo queue panel. Filtering hides
 * rows only — it never reorders, reselects, persists or fetches.
 *
 * Contract: trimmed lowercase substring over title/artist, compared after NFC
 * canonical normalization of metadata and query (the stable-id fallback stays
 * byte-literal, case fold only). Blank titles, including display titles explicitly
 * marked as synthesized, render (and match) as their stable track id, never empty;
 * artist text still compares canonically across the id/artist boundary. A missing
 * artist matches as empty, never the string "undefined". The raw query
 * is normalized here, so passing an already-normalized query is a no-op.
 * Pure reads only — the track is never mutated.
 */

import { canonicalMetadata, findNeedle, literalId } from '../library/canonical-compare.js';

// Bound the canonical normalization and mixed literal/canonical boundary search.
const MAX_FILTER_QUERY_LENGTH = 4096;

export interface QueueFilterTrack {
	id: string;
	title: string;
	/** True only when construction substituted the stable id for a missing title. */
	titleIsFallback?: boolean;
	artist?: string | null;
}

// Try each possible separator: ids and artists may themselves contain spaces.
// Only the artist part of a spanning query receives canonical normalization.
function matchesIdArtistBoundary(id: string, artist: string, query: string): boolean {
	for (let boundary = query.indexOf(' '); boundary !== -1 && boundary <= id.length; boundary = query.indexOf(' ', boundary + 1)) {
		if (
			id.endsWith(query.slice(0, boundary)) &&
			artist.startsWith(canonicalMetadata(query.slice(boundary + 1)))
		) return true;
	}
	return false;
}

export function matchesQueueQuery(track: QueueFilterTrack, query: string): boolean {
	const literal = typeof query === 'string' ? query.trim().toLowerCase() : null;
	if (literal === null || literal.length > MAX_FILTER_QUERY_LENGTH) return false;
	const needle = findNeedle(literal);
	if (!needle.literal) return true;
	if (typeof track.id !== 'string' || (track.title != null && typeof track.title !== 'string') ||
		(track.artist != null && typeof track.artist !== 'string')) return false;
	const artist = canonicalMetadata(track.artist ?? '');
	// An actual title may equal the id; only construction knows its provenance.
	if (track.title?.trim() && !track.titleIsFallback) {
		return `${canonicalMetadata(track.title)} ${artist}`.includes(needle.canonical);
	}
	const id = literalId(track.id);
	return id.includes(needle.literal) || artist.includes(needle.canonical) ||
		matchesIdArtistBoundary(id, artist, needle.literal);
}
