import { describe, expect, test } from 'vitest';
import type { LibraryItem } from './cache.js';
import { filterLibraryItems } from './filter.js';
import { groupLibraryItems } from './group.js';
import { orderLibraryItems } from './order.js';

/**
 * Library presentation pins over the CC0 fixture album (Holizna, "Busted
 * Guitar (JAZZ)"; see docs/fixture-audio-licences.md).
 *
 * The seed is derived from the committed manifest (the source of truth the
 * Rust tests also parse), so a manifest update cannot leave these
 * assertions running against outdated metadata.
 */
// Raw manifest via Vite glob: no node:fs, no @types/node needed.
const manifestRaw = import.meta.glob(
	'../../../../../fixtures/cc0/holiznacc0-busted-guitar-jazz-66ef0593/manifest.json',
	{ query: '?raw', import: 'default', eager: true }
) as Record<string, string>;
const manifest = JSON.parse(Object.values(manifestRaw)[0]) as {
	release: { title: string; artist: string };
	tracks: { position: number; title: string; recording_mbid: string; track_mbid: string }[];
};
const seed = [...manifest.tracks].sort((a, b) => a.position - b.position);

const album: LibraryItem[] = seed.map((track) => ({
	id: track.track_mbid,
	title: track.title,
	artist: manifest.release.artist,
	album: manifest.release.title,
	recordingMbid: track.recording_mbid
}));
const albumIds = album.map(({ id }) => id);
const ids = (rows: readonly LibraryItem[]) => rows.map(({ id }) => id);
const shuffled = [album[2], album[0], album[3], album[1]];

describe('CC0 fixture album in the library view', () => {
	test('seed comes from the manifest: four tracks in positions 1-4', () => {
		expect(seed.map((track) => track.position)).toEqual([1, 2, 3, 4]);
		expect(album).toHaveLength(4);
	});
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
		expect(ids(filterLibraryItems(album, album[2].title))).toEqual([albumIds[2]]);
	});

	test('find does not search album names (current gap)', () => {
		expect(filterLibraryItems(album, 'Busted Guitar')).toEqual([]);
	});
});
