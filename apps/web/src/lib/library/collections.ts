export type CollectionFilter = 'all' | 'albums' | 'artists';
export type CollectionSort = 'catalogue' | 'title' | 'creator';
export type Collection = {
	id: string;
	kind: 'Album' | 'Artist';
	title: string;
	creator: string;
	artPath: string | null;
};

// The member catalogue returns at most 50 rows per endpoint, with no cursor.
export const COLLECTION_LIMIT = 50;
const MBID = /^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$/i;

function parseCollections(value: unknown, kind: Collection['kind']): Collection[] {
	if (!Array.isArray(value) || value.length > COLLECTION_LIMIT) throw new Error('Invalid catalogue page');
	const seen = new Set<string>();
	return value.map((entry: unknown) => {
		if (typeof entry !== 'object' || entry === null) throw new Error('Invalid catalogue row');
		const { mbid, title, name, artist, art_url } = entry as Record<string, unknown>;
		const label = kind === 'Album' ? title : name;
		if (typeof mbid !== 'string' || !MBID.test(mbid) || typeof label !== 'string' || !label.trim()) throw new Error('Invalid catalogue row');
		const id = mbid.toLowerCase();
		if (seen.has(id)) throw new Error('Duplicate catalogue row');
		seen.add(id);
		const artPath = `/v1/releases/${mbid}/art`;
		return {
			id,
			kind,
			title: label,
			creator: kind === 'Album' && typeof artist === 'string' ? artist : '',
			// Accept only this release's NNTune serving path, never a publisher URL.
			artPath: kind === 'Album' && art_url === artPath ? artPath : null
		};
	});
}

export async function loadCollections(client: { get(path: string): Promise<unknown> }, filter: CollectionFilter): Promise<Collection[]> {
	const [albums, artists] = await Promise.all([
		filter === 'artists' ? [] : client.get('/v1/releases').then((rows) => parseCollections(rows, 'Album')),
		filter === 'albums' ? [] : client.get('/v1/artists').then((rows) => parseCollections(rows, 'Artist'))
	]);
	return [...albums, ...artists];
}

export function displayCollections(rows: readonly Collection[], query: string, sort: CollectionSort, reverse: boolean): Collection[] {
	const needle = query.trim().toLocaleLowerCase();
	const visible = rows.filter(({ title, creator }) => [title, creator].some((text) => text.toLocaleLowerCase().includes(needle)));
	if (sort !== 'catalogue') {
		const key = (row: Collection) => sort === 'creator' ? row.creator || row.title : row.title;
		visible.sort((a, b) => key(a).localeCompare(key(b)) || a.title.localeCompare(b.title) || a.id.localeCompare(b.id));
	}
	return reverse ? visible.reverse() : visible;
}
