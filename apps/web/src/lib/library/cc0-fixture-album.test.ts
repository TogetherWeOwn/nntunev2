import { describe, expect, test } from 'vitest';
import type { LibraryItem } from './cache.js';
import { filterLibraryItems } from './filter.js';
import { groupLibraryItems } from './group.js';
import { orderLibraryItems } from './order.js';

/**
 * Library presentation pins over the CC0 fixture album (Holizna, "Busted
 * Guitar (JAZZ)"; see docs/fixture-audio-licences.md).
 *
 * The seed mirrors fixtures/cc0/holiznacc0-busted-guitar-jazz-66ef0593/manifest.json
 * (CC0 fixture album, source of truth; Rust tests parse the manifest itself).
 */
const seed = [
	{
		position: 1,
		title: '1 (jazz)',
		recording_mbid: '35b1a0af-ac7e-4438-9116-b6da20f710fd',
		track_mbid: 'abadd874-288f-4ff2-b48c-96fafbf34b81'
	},
	{
		position: 2,
		title: '2 (jazz)',
		recording_mbid: '0323873d-b076-4dfe-aabb-c9e5dcc20a3b',
		track_mbid: 'cc6d2a01-b4b3-4172-8817-1bc2086c49af'
	},
	{
		position: 3,
		title: '3 (jazz)',
		recording_mbid: '58a4d716-bcef-4880-9b7e-8121863f1ce5',
		track_mbid: '7327490d-9f98-443f-ad0b-2ea477991920'
	},
	{
		position: 4,
		title: '4 (jazz)',
		recording_mbid: 'b8866ce2-68ef-475b-b82c-0df16083a708',
		track_mbid: 'b1152a6c-539c-4d8e-b45d-1349b5097779'
	}
];

const album: LibraryItem[] = seed.map((track) => ({
	id: track.track_mbid,
	title: track.title,
	artist: 'Holizna',
	album: 'Busted Guitar (JAZZ)',
	recordingMbid: track.recording_mbid
}));
const albumIds = album.map(({ id }) => id);
const ids = (rows: readonly LibraryItem[]) => rows.map(({ id }) => id);
const shuffled = [album[2], album[0], album[3], album[1]];

describe('CC0 fixture album in the library view', () => {
	test('groups into one Holizna group in album order', () => {
		const groups = groupLibraryItems(album);
		expect(groups).toHaveLength(1);
		expect(groups[0].key).toBe('holizna');
		expect(groups[0].label).toBe('Holizna');
		expect(ids(groups[0].items)).toEqual(albumIds);
	});

	test('title ordering yields position order, even from shuffled input', () => {
		expect(ids(orderLibraryItems(album, 'title'))).toEqual(albumIds);
		expect(ids(orderLibraryItems(shuffled, 'title'))).toEqual(albumIds);
	});

	test('artist ordering keeps the album together in input order', () => {
		expect(ids(orderLibraryItems(album, 'artist'))).toEqual(albumIds);
		expect(ids(orderLibraryItems(shuffled, 'artist'))).toEqual(ids(shuffled));
	});

	test('find matches by artist, by shared title text, and by a single title', () => {
		expect(ids(filterLibraryItems(album, 'holizna', 'artist'))).toEqual(albumIds);
		expect(ids(filterLibraryItems(album, 'jazz'))).toEqual(albumIds);
		expect(ids(filterLibraryItems(album, '3 (jazz)'))).toEqual([albumIds[2]]);
	});

	test('find does not search album names (current gap)', () => {
		expect(filterLibraryItems(album, 'Busted Guitar')).toEqual([]);
	});
});
