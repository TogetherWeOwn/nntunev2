import type { LibraryItem } from './cache.js';

export type LibraryOrder = 'original' | 'title' | 'artist';

export function displayTitle(track: Pick<LibraryItem, 'id' | 'title'>): string {
	const title = track.title;
	if (typeof title === 'string' && title.trim().length > 0) return title;
	return track.id;
}

export function displayArtist(track: Pick<LibraryItem, 'id' | 'artist'>): string | null {
	const artist = track.artist;
	return typeof artist === 'string' && artist.trim().length > 0 ? artist : null;
}

/**
 * Presentation only: never sort the fetched/cached array. Lowercase Unicode
 * keys compare by ECMAScript UTF-16 code units, not the host's locale. Stable
 * Array.sort retains API order for equal keys; no secondary key or metadata
 * normalization is applied. Unnamed artists follow every named artist.
 */
export function orderLibraryItems(items: readonly LibraryItem[], order: LibraryOrder): LibraryItem[] {
	const ordered = [...items];
	if (order === 'original') return ordered;
	const key = (track: LibraryItem): string | null =>
		order === 'title'
			? displayTitle(track).toLowerCase()
			: displayArtist(track)?.trim().toLowerCase() ?? null;
	return ordered.sort((left, right) => {
		const a = key(left);
		const b = key(right);
		if (a === b) return 0;
		if (a === null) return 1;
		if (b === null) return -1;
		return a < b ? -1 : 1;
	});
}
