/**
 * Shared queue-only presentation matcher: one implementation for the
 * library PlayerQueue panel and the demo queue panel. Filtering hides
 * rows only — it never reorders, reselects, persists or fetches.
 *
 * Contract: trimmed lowercase substring over title/artist. Blank titles
 * render (and match) as their stable track id, never empty; a missing
 * artist matches as empty, never the string "undefined". The raw query
 * is normalized here, so passing an already-normalized query is a no-op.
 * Pure reads only — the track is never mutated.
 */

export interface QueueFilterTrack {
	id: string;
	title: string;
	artist?: string | null;
}

export function matchesQueueQuery(track: QueueFilterTrack, query: string): boolean {
	const normalized = query.trim().toLowerCase();
	if (!normalized) return true;
	const title = track.title?.trim() ? track.title : track.id;
	return `${title} ${track.artist ?? ''}`.toLowerCase().includes(normalized);
}
