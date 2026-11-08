import { isMbid } from '../catalogue/catalogue.js';
import type { LibraryItem } from './cache.js';
import { displayArtist } from './order.js';

export interface EntityLinks {
	/** `/album/<release_mbid>` when the row carries a valid release MBID. */
	albumHref: string | null;
	/** `/artist/<artist_mbid>` when the row carries a valid artist MBID and a name. */
	artistHref: string | null;
}

function validId(value: unknown): string | null {
	return isMbid(value) ? value.toLowerCase() : null;
}

/**
 * Metadata-only: derives catalogue routes from the identifiers already on a
 * library row. Absent or malformed ids yield null so the caller keeps plain
 * text. Never resolves ids from the library list or any network call.
 */
export function entityLinks(item: LibraryItem): EntityLinks {
	const release = validId(item.release_mbid);
	const artist = validId(item.artist_mbid);
	return {
		albumHref: release ? `/album/${release}` : null,
		artistHref: artist && displayArtist(item) ? `/artist/${artist}` : null
	};
}
