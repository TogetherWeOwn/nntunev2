import { ApiError, type ApiClient } from '../api/client.js';

export type CollectionKind = 'all' | 'albums' | 'artists';
export type CollectionSort = 'original' | 'title' | 'creator';

export interface LibraryCollection {
	id: string;
	kind: 'album' | 'artist';
	title: string;
	creator: string;
	artUrl: string | null;
	year: number | null;
}

// The catalogue currently returns bounded first pages, not cursor pages.
export const COLLECTION_PAGE_SIZE = 50;
const MBID = '[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}';
const mbidPattern = new RegExp(`^${MBID}$`);
const artPattern = new RegExp(`^/v1/releases/(${MBID})/art$`);

/** Only the catalogue's reserved serving path, never a third-party art URL. */
export function collectionArtPath(value: unknown, releaseMbid?: string): string | null {
	if (typeof value !== 'string') return null;
	const match = artPattern.exec(value);
	return match && (releaseMbid === undefined || match[1] === releaseMbid) ? value : null;
}

function malformedCollection(): ApiError {
	return new ApiError(200, 'request_failed', 'Could not read the library collections.');
}

export function parseCollections(value: unknown, kind: 'album' | 'artist'): LibraryCollection[] {
	if (!Array.isArray(value) || value.length > COLLECTION_PAGE_SIZE) throw malformedCollection();
	const seen = new Set<string>();
	return value.map((entry: unknown) => {
		if (typeof entry !== 'object' || entry === null || Array.isArray(entry)) throw malformedCollection();
		const row = entry as Record<string, unknown>;
		const title = kind === 'album' ? row.title : row.name;
		if (typeof row.mbid !== 'string' || !mbidPattern.test(row.mbid) ||
			typeof title !== 'string' || title.trim() === '' || seen.has(row.mbid)) {
			throw malformedCollection();
		}
		seen.add(row.mbid);
		return {
			id: row.mbid,
			kind,
			title,
			creator: kind === 'album' && typeof row.artist === 'string' ? row.artist : '',
			artUrl: kind === 'album' ? collectionArtPath(row.art_url, row.mbid) : null,
			year: kind === 'album' && typeof row.year === 'number' && Number.isInteger(row.year) ? row.year : null
		};
	});
}

export async function loadCollections(
	client: Pick<ApiClient, 'get'>,
	kind: CollectionKind
): Promise<LibraryCollection[]> {
	const [albums, artists] = await Promise.all([
		kind !== 'artists' ? client.get<unknown>('/v1/releases').then((rows) => parseCollections(rows, 'album')) : [],
		kind !== 'albums' ? client.get<unknown>('/v1/artists').then((rows) => parseCollections(rows, 'artist')) : []
	]);
	return [...albums, ...artists];
}

/** Presentation only: filtering and sorting never refetch or reshape the queue. */
export function displayCollections(
	items: readonly LibraryCollection[], query: string, sort: CollectionSort, descending = false
): LibraryCollection[] {
	const needle = query.trim().toLocaleLowerCase();
	const filtered = items.filter((item) => `${item.title}\n${item.creator}`.toLocaleLowerCase().includes(needle));
	if (sort !== 'original') {
		filtered.sort((left, right) => {
			const a = sort === 'creator' ? left.creator || left.title : left.title;
			const b = sort === 'creator' ? right.creator || right.title : right.title;
			return a.localeCompare(b) || left.title.localeCompare(right.title) || left.id.localeCompare(right.id);
		});
	}
	return descending ? filtered.reverse() : filtered;
}
