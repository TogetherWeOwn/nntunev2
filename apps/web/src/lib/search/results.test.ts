import { describe, expect, it } from 'vitest';
import { mergeSearchResults, searchCategory, searchHref } from './results.js';
import type { CatalogueHit } from './catalogue.js';
import { resolveShareTarget } from '#lib/player/shareTimestamp.js';

const mbid = '11111111-1111-1111-1111-111111111111';
const hit = (kind: CatalogueHit['kind'], label = 'Coral'): CatalogueHit => ({ kind, mbid, label, score: 0.9,
	availability: 'available', destination: kind === 'artist' ? `/artist/${mbid}` : kind === 'release' ? `/album/${mbid}` : null,
	occurrences: [] });

describe('unified search results', () => {
	it('links library tracks to the working library destination without duplicate catalogue tracks', () => {
		const results = mergeSearchResults([{ id: 'track/a', title: 'Coral', recording_mbids: [mbid] }], [hit('recording')], 'Coral');
		expect(results).toHaveLength(1);
		expect(results[0]).toMatchObject({ category: 'tracks', href: '/library?track=track%2Fa&t=0', inLibrary: true });
		const params = new URL(results[0].href, 'https://example.test').searchParams;
		expect(resolveShareTarget({ track: params.get('track'), t: params.get('t') }, ['track/a'])).toEqual({ trackId: 'track/a', seconds: 0 });
	});
	it('hides recordings without authoritative library membership', () => {
		expect(mergeSearchResults([], [hit('recording')], 'Coral')).toEqual([]);
	});
	it('discovers only the server-authorized occurrence, not every row carrying the album array', () => {
		const recording = { ...hit('recording'), occurrences: [{ id: 'local', occurrence_key: 'job:2' }] };
		const results = mergeSearchResults([
			{ id: 'other', occurrence_key: 'job:1', title: 'Other Song', recording_mbids: [mbid] },
			{ id: 'local', occurrence_key: 'job:2', title: 'Reef', recording_mbids: [mbid] }
		], [recording], 'Coral');
		expect(results.map((result) => result.label)).toEqual(['Reef']);
	});
	it('does not infer ready media from album membership or a mismatched occurrence', () => {
		const items = [{ id: 'local', occurrence_key: 'job:2', title: 'Reef', recording_mbids: [mbid] }];
		expect(mergeSearchResults(items, [hit('recording')], 'Coral')).toEqual([]);
		expect(mergeSearchResults(items, [{ ...hit('recording'), occurrences: [{ id: 'local', occurrence_key: 'job:1' }] }], 'Coral')).toEqual([]);
	});
	it('never invents entity links when the server could not validate the destination', () => {
		expect(mergeSearchResults([], [{ ...hit('artist'), destination: null }, { ...hit('release'), destination: null }], 'Coral')).toEqual([]);
	});
	it('preserves separate library occurrences of a recording', () => {
		const results = mergeSearchResults([
			{ id: 'same', occurrence_key: 'first', title: 'Coral One' },
			{ id: 'same', occurrence_key: 'second', title: 'Coral Two' }
		], [], 'Coral');
		expect(results).toHaveLength(2);
		expect(new Set(results.map((result) => result.key)).size).toBe(2);
	});
	it('links artists and albums using the approved entity routes, without a false library badge', () => {
		const results = mergeSearchResults([], [hit('artist'), hit('release')], 'Coral');
		expect(results.map((result) => result.href)).toEqual([`/artist/${mbid}`, `/album/${mbid}`]);
		expect(results.every((result) => !result.inLibrary)).toBe(true);
	});
	it('does not show all library rows on the landing page', () => {
		expect(mergeSearchResults([{ id: 'a', title: 'Coral' }], [], ' ')).toEqual([]);
	});
	it.each(['.', '..'])('preserves dot-only query %s and category through URL normalization', (query) => {
		const url = new URL(searchHref(query, 'albums'), 'https://example.test');
		expect(url.pathname).toBe('/search');
		expect(url.searchParams.get('q')).toBe(query);
		expect(searchCategory(url.searchParams.get('type'))).toBe('albums');
	});
	it('encodes reserved query characters and bounds category selection', () => {
		expect(searchHref(' Coral / Reef? ', 'albums')).toBe('/search/Coral%20%2F%20Reef%3F/albums');
		expect(searchHref(' ')).toBe('/search');
		expect(searchCategory('artists')).toBe('artists');
		expect(searchCategory('not-a-category')).toBe('all');
	});
});
