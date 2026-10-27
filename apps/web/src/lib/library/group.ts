import type { LibraryItem } from './cache.js';

export interface ArtistGroup {
	key: string | null;
	label: string;
	items: LibraryItem[];
}

/** Locale-independent UTF-16 comparison of trimmed, lowercase artist keys. */
function compareArtistKeys(a: string | null, b: string | null): number {
	if (a === b) return 0;
	if (a === null) return 1;
	if (b === null) return -1;
	return a < b ? -1 : 1;
}

/** Stable partition of the displayed view; never sorts or edits the input rows. */
export function groupLibraryItems(items: readonly LibraryItem[]): ArtistGroup[] {
	const groups = new Map<string | null, ArtistGroup>();
	for (const track of items) {
		const artist = typeof track.artist === 'string' ? track.artist.trim() : '';
		const key = artist ? artist.toLowerCase() : null;
		let group = groups.get(key);
		if (!group) {
			group = { key, label: artist || 'Unknown artist', items: [] };
			groups.set(key, group);
		}
		group.items.push(track);
	}
	return [...groups.values()].sort((a, b) => compareArtistKeys(a.key, b.key));
}
