import { describe, expect, it } from 'vitest';
import { ApiClient, ApiError } from '../api/client.js';
import { collectionArtPath, displayCollections, loadCollections, parseCollections } from './collections.js';

const A = '11111111-1111-4111-8111-111111111111';
const B = '22222222-2222-4222-8222-222222222222';
const album = { mbid: A, title: 'Zulu', artist: 'North', year: 2024, art_url: `/v1/releases/${A}/art` };
const artist = { mbid: B, name: 'Azure', sort_name: 'Azure', type: 'Group', country: null };

function clientFor(routes: Record<string, unknown>, calls: string[] = []): ApiClient {
	return new ApiClient({ baseUrl: '', fetchFn: async (input, init) => {
		const path = String(input);
		calls.push(path);
		expect(init?.credentials).toBe('include');
		return new Response(JSON.stringify(routes[path]), { headers: { 'content-type': 'application/json' } });
	} });
}

describe('member library collections', () => {
	it('reads both collection endpoints for All and keeps their distinct entity identities', async () => {
		const calls: string[] = [];
		const rows = await loadCollections(clientFor({ '/v1/releases': [album], '/v1/artists': [artist] }, calls), 'all');
		expect(calls.sort()).toEqual(['/v1/artists', '/v1/releases']);
		expect(rows).toEqual([
			{ id: A, kind: 'album', title: 'Zulu', creator: 'North', year: 2024, artUrl: album.art_url },
			{ id: B, kind: 'artist', title: 'Azure', creator: '', year: null, artUrl: null }
		]);
	});

	it.each(['albums', 'artists'] as const)('requests only the %s collection', async (kind) => {
		const calls: string[] = [];
		await loadCollections(clientFor({ '/v1/releases': [album], '/v1/artists': [artist] }, calls), kind);
		expect(calls).toEqual([kind === 'albums' ? '/v1/releases' : '/v1/artists']);
	});

	it('preserves real empty collections rather than inventing content', () => {
		expect(parseCollections([], 'album')).toEqual([]);
	});

	it.each([null, {}, { items: [album] }, [null], [{ ...album, mbid: '../escape' }],
		[{ ...album, title: '' }], [album, album], Array.from({ length: 51 }, () => album)])(
		'refuses malformed or over-page payload %#', (payload) => {
			expect(() => parseCollections(payload, 'album')).toThrow(ApiError);
		}
	);

	it('does not convert a refused or malformed sibling request into a partial All success', async () => {
		await expect(loadCollections(clientFor({ '/v1/releases': [album], '/v1/artists': {} }), 'all')).rejects.toThrow(ApiError);
		const refused = new ApiClient({ baseUrl: '', fetchFn: async () => new Response('{}', { status: 403 }) });
		await expect(loadCollections(refused, 'albums')).rejects.toMatchObject({ status: 403 });
	});

	it('filters/sorts a copy of loaded rows without changing catalogue or queue order', () => {
		const rows = [...parseCollections([album], 'album'), ...parseCollections([artist], 'artist')];
		expect(displayCollections(rows, ' NORTH ', 'title').map((row) => row.id)).toEqual([A]);
		expect(displayCollections(rows, '', 'title').map((row) => row.id)).toEqual([B, A]);
		expect(displayCollections(rows, '', 'creator', true).map((row) => row.id)).toEqual([A, B]);
		expect(displayCollections(rows, '', 'original', true).map((row) => row.id)).toEqual([B, A]);
		expect(rows.map((row) => row.id)).toEqual([A, B]);
	});
});

describe('approved artwork serving path', () => {
	it('accepts only a matching release path', () => {
		expect(collectionArtPath(album.art_url, A)).toBe(album.art_url);
		expect(collectionArtPath(album.art_url, B)).toBeNull();
	});

	it.each([null, '', '//outside.invalid/art', 'https://outside.invalid/art', '/placeholder.svg',
		`/v1/releases/${A}/art?url=external`, `/v1/releases/${A}/art/extra`,
		`/v1/releases/${A}/../art`, 'data:image/svg+xml,test', 'blob:untrusted'])('rejects %s', (src) => {
		expect(collectionArtPath(src)).toBeNull();
	});
});
