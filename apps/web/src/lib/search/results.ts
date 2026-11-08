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
	const recordingIds = new Set(items.flatMap((item) =>
		Array.isArray(item.recording_mbids)
			? item.recording_mbids.filter((id): id is string => typeof id === 'string') : []
	));
	for (const hit of hits) {
		if (hit.kind === 'recording') {
			// A recording match can discover a differently named library row,
			// but an unindexed recording stays hidden (no invented availability).
			if (!recordingIds.has(hit.mbid)) continue;
			for (const item of items.filter((row) => Array.isArray(row.recording_mbids) && row.recording_mbids.includes(hit.mbid))) {
				if (results.some((result) => result.key === `library:${libraryItemKey(item)}`)) continue;
				results.push({ key: `library:${libraryItemKey(item)}`, category: 'tracks', label: displayTitle(item),
					artist: displayArtist(item) ?? undefined, href: `/library?${buildShareQuery(item.id, 0)}`, inLibrary: true });
			}
		} else {
			const category = hit.kind === 'artist' ? 'artists' : 'albums';
			const field = hit.kind === 'artist' ? 'artist_mbid' : 'release_mbid';
			results.push({ key: `${hit.kind}:${hit.mbid}`, category, label: hit.label,
				artist: hit.artistName, href: `/${hit.kind === 'artist' ? 'artist' : 'album'}/${hit.mbid}`,
				inLibrary: items.some((item) => item[field] === hit.mbid) });
		}
	}
	return [...new Map(results.map((result) => [result.key, result])).values()];
}
