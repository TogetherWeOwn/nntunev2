import { describe, expect, it, vi } from 'vitest';
import { ApiClient } from '../api/client.js';
import type { Release, Track } from './catalogue.js';
import {
	occurrenceHref, parseOccurrence, readOccurrence, releaseOccurrenceLinks, selectOccurrence, trackOccurrenceHref
} from './occurrence.js';

const rel = 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa';
const other = 'eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee';
const rec = 'cccccccc-cccc-cccc-cccc-cccccccccccc';
const rec2 = 'dddddddd-dddd-dddd-dddd-dddddddddddd';
const release: Release = {
	mbid: rel, title: 'Quiet Rooms', release_type: 'Album', year: 2024,
	artist_mbid: null, artist: null, art_url: null
};
const mk = (over: Partial<Track> = {}): Track => ({
	mbid: rec, title: 'First Light', release_mbid: rel, release: release.title,
	artist_mbid: null, artist: null, duration_ms: 90_000, medium_no: 1, track_no: 1, art_url: null, ...over
});
const q = (s: string) => new URLSearchParams(s);
const base = `release=${rel}&medium=1&position=1`;

function clientWith(body: unknown, status = 200) {
	const fetchFn = vi.fn<typeof fetch>().mockImplementation(async () =>
		new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } }));
	return { client: new ApiClient({ baseUrl: 'https://api.example.test', fetchFn }), fetchFn };
}

describe('occurrence urls', () => {
	it('encodes canonical lowercase links with explicit unknown parts', () => {
		expect(occurrenceHref({ recording: rec.toUpperCase(), release: rel, medium: 2, position: 0 }))
			.toBe(`/track/${rec}?release=${rel}&medium=2&position=0`);
		expect(trackOccurrenceHref(mk({ medium_no: null, track_no: null })))
			.toBe(`/track/${rec}?release=${rel}&medium=unknown&position=unknown`);
	});
	it('refuses to build links from invalid parts', () => {
		expect(occurrenceHref({ recording: 'x', release: rel, medium: 1, position: 1 })).toBeNull();
		expect(occurrenceHref({ recording: rec, release: 'x', medium: 1, position: 1 })).toBeNull();
		expect(occurrenceHref({ recording: rec, release: rel, medium: -1, position: 1 })).toBeNull();
		expect(occurrenceHref({ recording: rec, release: rel, medium: 1, position: 1.5 })).toBeNull();
	});
	it('does not link ambiguous occurrences but preserves repeated recordings on different discs', () => {
		const first = mk();
		const duplicate = mk();
		const secondDisc = mk({ medium_no: 2 });
		const links = releaseOccurrenceLinks([first, duplicate, secondDisc]);
		expect(links.has(first)).toBe(false);
		expect(links.has(duplicate)).toBe(false);
		expect(links.get(secondDisc)).toBe(`/track/${rec}?release=${rel}&medium=2&position=1`);
	});
	it('round-trips through the parser', () => {
		const key = { recording: rec, release: rel, medium: null, position: 7 };
		const href = occurrenceHref(key)!;
		expect(parseOccurrence(rec, q(href.split('?')[1]))).toEqual(key);
	});
});

describe('parseOccurrence', () => {
	it('accepts integers and unknown, normalizing case', () => {
		expect(parseOccurrence(rec.toUpperCase(), q(`release=${rel.toUpperCase()}&medium=unknown&position=0`)))
			.toEqual({ recording: rec, release: rel, medium: null, position: 0 });
		expect(parseOccurrence(rec, base)).not.toBeNull();
	});
	it.each([
		'', `medium=1&position=1`, `release=${rel}&position=1`, `release=${rel}&medium=1`,
		`release=bad&medium=1&position=1`, `release=${rel}&medium=&position=1`,
		`release=${rel}&medium=-1&position=1`, `release=${rel}&medium=01&position=1`,
		`release=${rel}&medium=1.5&position=1`, `release=${rel}&medium=1e1&position=1`,
		`release=${rel}&medium=+1&position=1`, `release=${rel}&medium=%201&position=1`,
		`release=${rel}&medium=99999999999999999999&position=1`,
		`release=${rel}&medium=Unknown&position=1`, `release=${rel}&medium=null&position=1`,
		`${base}&release=${other}`, `${base}&medium=2`, `${base}&position=2`,
		`release=${rel}%2F..&medium=1&position=1`
	])('rejects %s', (s) => expect(parseOccurrence(rec, q(s))).toBeNull());
	it.each(['', 'nope', `${rec}/x`, '../admin'])('rejects recording %s', (id) => {
		expect(parseOccurrence(id, q(base))).toBeNull();
	});
	it('rejects a non-string recording', () => expect(parseOccurrence(undefined, q(base))).toBeNull());
});

describe('selectOccurrence', () => {
	const key = { recording: rec, release: rel, medium: 1, position: 1 };
	it('keeps repeated recordings on distinct discs apart', () => {
		const tracks = [mk({ medium_no: 1, track_no: 1, title: 'd1' }), mk({ medium_no: 2, track_no: 1, title: 'd2' })];
		expect(selectOccurrence(tracks, key)?.title).toBe('d1');
		expect(selectOccurrence(tracks, { ...key, medium: 2 })?.title).toBe('d2');
	});
	it('matches null positions only against unknown, never a number', () => {
		const tracks = [mk({ medium_no: null, track_no: null }), mk({ mbid: rec2, medium_no: 1, track_no: 1 })];
		expect(selectOccurrence(tracks, { ...key, medium: null, position: null })).toBe(tracks[0]);
		expect(selectOccurrence([mk({ medium_no: null })], key)).toBeNull();
		expect(selectOccurrence([mk()], { ...key, position: null })).toBeNull();
	});
	it('fails closed on duplicates, mismatch and foreign release', () => {
		expect(selectOccurrence([mk(), mk({ title: 'dup' })], key)).toBeNull();
		expect(selectOccurrence([mk()], { ...key, position: 2 })).toBeNull();
		expect(selectOccurrence([mk()], { ...key, recording: rec2 })).toBeNull();
		expect(selectOccurrence([mk({ release_mbid: other })], key)).toBeNull();
	});
	it('accepts unknown duration', () => {
		expect(selectOccurrence([mk({ duration_ms: null })], key)?.duration_ms).toBeNull();
	});
});

describe('readOccurrence', () => {
	const gen = () => 3;
	it('makes exactly one release read and returns the exact track and release', async () => {
		const { client, fetchFn } = clientWith({ release, tracks: [mk({ medium_no: 2 }), mk(), mk({ mbid: rec2 })] });
		const out = await readOccurrence(client, rec.toUpperCase(), q(base), gen);
		expect(out).toEqual({ value: { track: mk(), release }, failure: null, authGeneration: 3 });
		expect(fetchFn).toHaveBeenCalledOnce();
		expect(fetchFn.mock.calls[0][0]).toBe(`https://api.example.test/v1/releases/${rel}`);
	});
	it('rejects malformed input before any request', async () => {
		const { client, fetchFn } = clientWith({ release, tracks: [mk()] });
		for (const s of ['', `${base}&release=${other}`, `release=${rel}&medium=x&position=1`]) {
			expect(await readOccurrence(client, rec, q(s), gen)).toEqual({ value: null, failure: 'not-found', authGeneration: 3 });
		}
		expect((await readOccurrence(client, 'bad', q(base), gen)).failure).toBe('not-found');
		expect(fetchFn).not.toHaveBeenCalled();
	});
	it('reports not-found for missing, mismatched or ambiguous occurrences', async () => {
		expect((await readOccurrence(clientWith({ release, tracks: [] }).client, rec, q(base), gen)).failure).toBe('not-found');
		expect((await readOccurrence(clientWith({ release, tracks: [mk()] }).client, rec, q(`release=${rel}&medium=1&position=2`), gen)).failure).toBe('not-found');
		expect((await readOccurrence(clientWith({ release, tracks: [mk(), mk()] }).client, rec, q(base), gen)).failure).toBe('not-found');
	});
	it('refuses a release response for another entity', async () => {
		const body = { release: { ...release, mbid: other }, tracks: [mk({ release_mbid: other })] };
		expect((await readOccurrence(clientWith(body).client, rec, q(base), gen)).failure).toBe('unavailable');
	});
	it.each([[401, 'signed-out'], [404, 'not-found'], [503, 'unavailable']] as const)('maps HTTP %i', async (status, failure) => {
		expect(await readOccurrence(clientWith({}, status).client, rec, q(base), gen)).toEqual({ value: null, failure, authGeneration: 3 });
	});
	it('discards a response that arrives after the auth generation changed', async () => {
		let current = 1;
		const { client } = clientWith({ release, tracks: [mk()] });
		const pending = readOccurrence(client, rec, q(base), () => current);
		current = 2;
		expect(await pending).toEqual({ value: null, failure: 'signed-out', authGeneration: 1 });
	});
	it('keeps a response when the generation is unchanged', async () => {
		const out = await readOccurrence(clientWith({ release, tracks: [mk()] }).client, rec, q(base), () => 1);
		expect(out.failure).toBeNull();
	});
});
