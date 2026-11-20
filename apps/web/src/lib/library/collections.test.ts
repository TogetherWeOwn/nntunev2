import { describe, expect, it, vi } from 'vitest';
import { COLLECTION_LIMIT, displayCollections, loadCollections } from './collections.js';

const id = 'abcdef00-0000-4000-8000-000000000001';
const path = `/v1/releases/${id}/art`;
const release = { mbid: id, title: 'Evening Radio', artist: 'North', art_url: path };

function clientFor(albums: unknown, artists: unknown = []) {
	return { get: vi.fn(async (url: string) => url === '/v1/releases' ? albums : artists) };
}

describe('member collections', () => {
	it('loads selected types, keeping album and artist IDs separate', async () => {
		const client = clientFor([release], [{ mbid: id, name: 'North', art_url: path }]);
		expect(await loadCollections(client, 'artists')).toEqual([
			{ id, kind: 'Artist', title: 'North', creator: '', artPath: null }
		]);
		expect(client.get.mock.calls).toEqual([['/v1/artists']]);
		expect((await loadCollections(client, 'all')).map((row) => row.kind)).toEqual(['Album', 'Artist']);
	});

	it('accepts only the exact matching release art path', async () => {
		for (const art_url of [`https://art.invalid${path}`, `${path}?variant=2`, path.replace('0001/art', '0002/art')]) {
			expect((await loadCollections(clientFor([{ ...release, art_url }]), 'albums'))[0].artPath).toBeNull();
		}
		expect((await loadCollections(clientFor([release]), 'albums'))[0].artPath).toBe(path);
	});

	it('rejects oversized, malformed and duplicate pages', async () => {
		for (const payload of [{ items: [] }, [null], [release, { ...release, mbid: id.toUpperCase() }], Array(COLLECTION_LIMIT + 1).fill(release)]) {
			await expect(loadCollections(clientFor(payload), 'albums')).rejects.toThrow();
		}
	});

	it('sorts and filters loaded rows without changing catalogue order', async () => {
		const rows = await loadCollections(clientFor([release, { ...release, mbid: id.replace(/1$/, '2'), title: 'Aria', artist: 'South' }]), 'albums');
		expect(displayCollections(rows, '', 'title', false).map((row) => row.title)).toEqual(['Aria', 'Evening Radio']);
		expect(displayCollections(rows, ' NORTH ', 'creator', true)).toEqual([rows[0]]);
		expect(displayCollections(rows, '', 'catalogue', true)).toEqual([rows[1], rows[0]]);
		expect(rows[0].title).toBe('Evening Radio');
	});
});
