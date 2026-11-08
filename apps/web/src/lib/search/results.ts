import { libraryItemKey, type LibraryItem } from '#lib/library/cache.js';
import { filterLibraryItems } from '#lib/library/filter.js';
import { displayArtist, displayTitle } from '#lib/library/order.js';
import { buildShareQuery } from '#lib/player/shareTimestamp.js';
import type { CatalogueHit } from './catalogue.js';

export const SEARCH_CATEGORIES = ['all', 'tracks', 'artists', 'albums', 'playlists', 'videos'] as const;
export type SearchCategory = (typeof SEARCH_CATEGORIES)[number];
export interface SearchResult {
	key: string;
	category: Exclude<SearchCategory, 'all'>;
	label: string;
	artist?: string;
	href: string;
	inLibrary: boolean;
}

export function searchCategory(value: string | null | undefined): SearchCategory {
	return SEARCH_CATEGORIES.find((category) => category === value) ?? 'all';
}

export function searchHref(query: string, category: SearchCategory = 'all'): string {
	const trimmed = query.trim();
	// URL parsers normalize both literal and percent-encoded dot segments.
	if (trimmed === '.' || trimmed === '..') {
		return `/search?${new URLSearchParams({ q: trimmed, type: category })}`;
	}
	return trimmed ? `/search/${encodeURIComponent(trimmed)}/${category}` : '/search';
}

/** Metadata alone is not evidence that a recording is playable. */
export function mergeSearchResults(
	items: readonly LibraryItem[],
	hits: readonly CatalogueHit[],
	query: string
): SearchResult[] {
	if (!query.trim()) return [];
	const results: SearchResult[] = filterLibraryItems([...items], query).map((item) => ({
		key: `library:${libraryItemKey(item)}`,
		category: 'tracks',
		label: displayTitle(item),
		artist: displayArtist(item) ?? undefined,
		href: `/library?${buildShareQuery(item.id, 0)}`,
		inLibrary: true
	}));
	for (const hit of hits) {
		if (hit.availability !== 'available') continue;
		if (hit.kind === 'recording') {
			// The ledger's recording_mbids is a whole album array on EVERY row.
			// Only server-authorized playable occurrences identify this hit; never
			// promote all of an album's rows from one recording's metadata match.
			for (const occurrence of hit.occurrences) {
				const item = items.find((row) => row.id === occurrence.id && row.occurrence_key === occurrence.occurrence_key);
				if (!item) continue;
				const key = `library:${libraryItemKey(item)}`;
				if (results.some((result) => result.key === key)) continue;
				results.push({ key, category: 'tracks', label: displayTitle(item),
					artist: displayArtist(item) ?? undefined, href: `/library?${buildShareQuery(item.id, 0)}`, inLibrary: true });
			}
		} else if (hit.destination) {
			const category = hit.kind === 'artist' ? 'artists' : 'albums';
			const field = hit.kind === 'artist' ? 'artist_mbid' : 'release_mbid';
			results.push({ key: `${hit.kind}:${hit.mbid}`, category, label: hit.label,
				artist: hit.artistName, href: hit.destination,
				inLibrary: items.some((item) => item[field] === hit.mbid) });
		}
	}
	return [...new Map(results.map((result) => [result.key, result])).values()];
}
