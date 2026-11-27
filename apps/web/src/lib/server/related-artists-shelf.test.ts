import { expect, it } from 'vitest';
import { selectDiscoveryTracks, type DiscoveryCandidate } from './discovery-shelf.js';
import { selectRelatedArtists } from './related-artists-shelf.js';

it('requires well-formed public mirror evidence for both shelves', () => {
	const mbid = (n: number) => `aaaaaaaa-1111-2222-3333-44444444444${n}`;
	const [id, seed, artist] = [mbid(1), mbid(2), mbid(3)];
	const row = { user_id: null, id, release_mbid: mbid(4), recording_mbids: [id], track_no: 0, title: 'Track', artist: null };
	const bad = [{ ...row, user_id: 'private' }, { ...row, track_no: -1 }, { ...row, release_mbid: 'unknown' },
		{ ...row, recording_mbids: id as unknown as DiscoveryCandidate['recording_mbids'] }];
	const names = new Map([[seed, 'Seed'], [artist, 'Artist'], ['invalid', 'Unverified']]);
	const links = [seed, artist, 'invalid'].map((artistMbid) => ({ artistMbid, recordingMbid: id, linkType: 'credit' }));
	expect(selectDiscoveryTracks(bad)).toEqual([]);
	expect(selectRelatedArtists(seed, bad, names, links)).toEqual([]);
	expect(selectRelatedArtists(seed, [...bad, row], names, links)).toEqual([{ id: artist, name: 'Artist' }]);
});
