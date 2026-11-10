import { describe, expect, it } from 'vitest';
import { entityLinks } from './entity-links.js';

const release = 'AAAAAAAA-aaaa-aaaa-aaaa-aaaaaaaaaaaa';
const artist = 'bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb';

describe('library entity links', () => {
	it('builds lowercase album and artist routes from valid ids', () => {
		expect(entityLinks({ id: 't', artist: 'North', release_mbid: release, artist_mbid: artist })).toEqual({
			albumHref: `/album/${release.toLowerCase()}`,
			artistHref: `/artist/${artist}`
		});
	});

	it('keeps absent, malformed or non-string ids as text', () => {
		for (const bad of [undefined, null, '', 'not-an-id', '../x', `${artist}/extra`, 42, {}]) {
			expect(entityLinks({ id: 't', artist: 'North', release_mbid: bad, artist_mbid: bad })).toEqual({
				albumHref: null,
				artistHref: null
			});
		}
	});

	it('does not link an artist without a displayable name', () => {
		expect(entityLinks({ id: 't', artist: '  ', artist_mbid: artist }).artistHref).toBeNull();
		expect(entityLinks({ id: 't', artist_mbid: artist }).artistHref).toBeNull();
	});
});
