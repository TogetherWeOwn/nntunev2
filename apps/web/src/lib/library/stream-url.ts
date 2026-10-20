/**
 * Resolve the playable stream URL for the queue's current track.
 *
 * Returns the trimmed `streamUrl`, or `''` when there is no track or no
 * usable URL (missing, non-string, empty or whitespace-only). The library
 * route renders `<audio>` only for a non-empty result, so a whitespace-only
 * URL must not slip through: `src="   "` resolves to the page URL as media,
 * the same failure an empty `src` caused.
 */
export function resolveStreamUrl(track: { streamUrl?: unknown } | null | undefined): string {
	const raw = track?.streamUrl;
	if (typeof raw !== 'string') return '';
	return raw.trim();
}
