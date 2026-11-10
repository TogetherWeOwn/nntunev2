import { describe, expect, it, vi } from 'vitest';
import { ApiClient } from '../api/client.js';
import {
	discTracks, filterReleases, groupReleases, isArtistDetail, isReleaseDetail,
	readArtist, readRelease, releaseArtwork, releaseDuration, trackDuration,
	type Release, type Track
} from './catalogue.js';

const albumId = 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa';
const artistId = 'bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb';
const trackId = 'cccccccc-cccc-cccc-cccc-cccccccccccc';
const groupId = 'dddddddd-dddd-dddd-dddd-dddddddddddd';
const release: Release = {
	mbid: albumId, title: 'Quiet Rooms', release_type: 'Album', year: 2024,
	artist_mbid: artistId, artist: 'North Window', art_url: `/v1/releases/${albumId}/art`, release_group_mbid: groupId
};
const track: Track = {
	mbid: trackId, title: 'First Light', release_mbid: albumId, release: release.title,
	artist_mbid: artistId, artist: release.artist, duration_ms: 90_000, medium_no: 1, track_no: 1, art_url: release.art_url
};
const artist = { mbid: artistId, name: 'North Window', sort_name: 'North Window', type: 'Group', country: null };

function clientWith(body: unknown, status = 200) {
	const fetchFn = vi.fn<typeof fetch>().mockResolvedValue(new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } }));
	return { client: new ApiClient({ baseUrl: 'https://api.example.test', fetchFn }), fetchFn };
}

describe('catalogue reads', () => {
	it('reads normalized identity through the credentialed API client', async () => {
		const body = { release, tracks: [track] };
		const { client, fetchFn } = clientWith(body);
		expect(await readRelease(client, albumId.toUpperCase())).toEqual({ value: body, failure: null });
		expect(fetchFn).toHaveBeenCalledWith(`https://api.example.test/v1/releases/${albumId}`, expect.objectContaining({ credentials: 'include', method: 'GET' }));
	});

	it('reads artist identity directly, not from a capped list', async () => {
		const body = { artist, releases: [release], truncated: false };
		const { client, fetchFn } = clientWith(body);
		expect(await readArtist(client, artistId)).toEqual({ value: body, failure: null });
		expect(fetchFn).toHaveBeenCalledOnce();
		expect(fetchFn.mock.calls[0][0]).toBe(`https://api.example.test/v1/artists/${artistId}`);
	});

	it.each(['../admin', '', 'not-an-id', `${albumId}/art`])('rejects malformed identity %s without a request', async (id) => {
		const { client, fetchFn } = clientWith({});
		expect(await readRelease(client, id)).toEqual({ value: null, failure: 'not-found' });
		expect(await readArtist(client, id)).toEqual({ value: null, failure: 'not-found' });
		expect(fetchFn).not.toHaveBeenCalled();
	});

	it.each([[401, 'signed-out'], [404, 'not-found'], [503, 'unavailable']] as const)('keeps HTTP %i distinct from an empty release', async (status, failure) => {
		const { client } = clientWith({ error: { code: 'test', message: 'private diagnostic' } }, status);
		expect(await readRelease(client, albumId)).toEqual({ value: null, failure });
	});

	it('refuses malformed and wrong-entity successful responses', async () => {
		for (const body of [null, {}, { release, tracks: [null] }, { release, tracks: [{ ...track, release_mbid: artistId }] }, { release: { ...release, mbid: artistId }, tracks: [] }]) {
			expect((await readRelease(clientWith(body).client, albumId)).failure).toBe('unavailable');
		}
		expect((await readArtist(clientWith({ artist: { ...artist, mbid: albumId }, releases: [], truncated: false }).client, artistId)).failure).toBe('unavailable');
	});

	it('bounds responses and validates nullable fields without inventing zero values', () => {
		expect(isReleaseDetail({ release, tracks: [{ ...track, duration_ms: null, medium_no: null }] })).toBe(true);
		expect(isReleaseDetail({ release, tracks: [{ ...track, duration_ms: -1 }] })).toBe(false);
		expect(isReleaseDetail({ release, tracks: Array(1001).fill(track) })).toBe(false);
		expect(isArtistDetail({ artist, releases: Array(51).fill(release), truncated: true })).toBe(false);
		expect(isArtistDetail({ artist, releases: [{ ...release, artist_mbid: albumId }], truncated: false })).toBe(false);
	});
});

describe('catalogue presentation', () => {
	it('preserves repeated recordings on different discs, using real positions', () => {
		const second = { ...track, medium_no: 2, track_no: 1 };
		const last = { ...track, medium_no: null, track_no: null };
		expect(discTracks([second, last, track])).toEqual([
			{ medium: 1, tracks: [track] }, { medium: 2, tracks: [second] }, { medium: null, tracks: [last] }
		]);
	});

	it('never presents partial duration as total', () => {
		expect(releaseDuration([track, { ...track, duration_ms: null }])).toBeNull();
		expect(releaseDuration([])).toBeNull();
		expect(releaseDuration([track, track])).toBe('3:00');
		expect(trackDuration(null)).toBe('Unknown');
		expect(trackDuration(0)).toBe('0:00');
	});

	it('groups only known release-group identities, not titles', () => {
		const edition = { ...release, mbid: trackId };
		const unknown = { ...release, mbid: artistId, release_group_mbid: null };
		expect(groupReleases([release, edition, unknown])).toEqual([[release, edition], [unknown]]);
	});

	it('keeps compilation releases out of Albums and unknown types out of invented categories', () => {
		const compilation = { ...release, mbid: trackId, release_type: 'Album + Compilation' };
		const unknown = { ...release, mbid: artistId, release_type: null };
		const ep = { ...release, release_type: 'EP' };
		const all = [release, compilation, unknown, ep];
		expect(filterReleases(all, 'albums')).toEqual([release]);
		expect(filterReleases(all, 'compilations')).toEqual([compilation]);
		expect(filterReleases(all, 'singles')).toEqual([ep]);
		expect(filterReleases(all, 'all')).toEqual(all);
	});

	it('allows only the reserved artwork path through the configured API origin', () => {
		expect(releaseArtwork(release, 'https://api.example.test/')).toBe(`https://api.example.test/v1/releases/${albumId}/art`);
		for (const art_url of ['https://art.example.test/cover', '//art.example.test/cover', '/api/demo/artwork', `${release.art_url}?redirect=1`, null]) {
			expect(releaseArtwork({ ...release, art_url }, '')).toBeNull();
		}
	});
});
