import { render } from 'svelte/server';
import { expect, test } from 'vitest';
import type { DiscoveryCandidate } from '../server/discovery-shelf.js';
import { selectRelatedArtists, type ArtistRecordingRelationship } from '../server/related-artists-shelf.js';
import RelatedArtistsShelf from './RelatedArtistsShelf.svelte';

const seed = '11111111-1111-4111-8111-111111111111';
const firstArtist = '22222222-2222-4222-8222-222222222222';
const firstRecording = '88888888-8888-4888-8888-888888888888';
const secondRecording = 'dddddddd-dddd-4ddd-8ddd-dddddddddddd';
const syntheticMbid = (index: number) => `aaaaaaaa-1111-4111-8111-${String(index).padStart(12, '0')}`;

function relationship(artistMbid: string, recordingMbid: string, linkType = 'credit'): ArtistRecordingRelationship {
	return { artistMbid, recordingMbid, linkType };
}

// Decoded ArtistRecording golden from mb-import-validate/tests/relationships.rs.
const normalizedRelationships = [
	relationship(seed, firstRecording, 'arrangement'),
	relationship(seed, firstRecording),
	relationship(seed, secondRecording),
	relationship(firstArtist, firstRecording)
];

function recording(id: string, overrides: Partial<DiscoveryCandidate> = {}): DiscoveryCandidate {
	return {
		user_id: null,
		id,
		release_mbid: 'bbbbbbbb-1111-4111-8111-444444444444',
		recording_mbids: [id],
		track_no: 1,
		title: 'Fixture Recording',
		artist: 'Fixture Credit',
		...overrides
	};
}

test('ranks distinct public recording overlaps with deterministic ties and a five-artist cap', () => {
	const others = Array.from({ length: 6 }, (_, index) => syntheticMbid(index));
	const leader = others[5];
	const names = new Map([
		[seed, 'Seed Artist'],
		[firstArtist, 'Zulu Artist'],
		...others.map((id, i): [string, string] => [id, i === 5 ? 'Bough & Root' : `Artist ${i}`])
	]);
	const upper = secondRecording.toUpperCase();
	const catalogue = [
		...others.map((_, index) => recording(syntheticMbid(index + 100))),
		recording(firstRecording),
		recording(upper),
		recording(secondRecording)
	];
	const relationships = [
		...normalizedRelationships,
		...others.map((artist) => relationship(artist, firstRecording)),
		relationship(firstArtist, firstRecording, 'arrangement'),
		relationship(firstArtist, firstRecording),
		relationship(leader, secondRecording)
	];
	const expected = [leader, firstArtist, ...others.slice(0, 3)].map((id) => ({ id, name: names.get(id) }));
	const selected = selectRelatedArtists(seed, catalogue, names, relationships);
	expect(selected).toEqual(expected);
	expect(selectRelatedArtists(
		seed, [...catalogue].reverse(), new Map([...names].reverse()), [...relationships].reverse()
	)).toEqual(expected);

	const html = render(RelatedArtistsShelf, { props: { artists: selected } }).body;
	expect([...html.matchAll(/<li>(.*?)<\/li>/g)].map(([, label]) => label)).toEqual([
		'Bough &amp; Root', 'Zulu Artist', 'Artist 0', 'Artist 1', 'Artist 2'
	]);
});

test('excludes private and unknown candidates before computing rank', () => {
	const privateArtist = syntheticMbid(1);
	const unknownArtist = syntheticMbid(2);
	const publicLeader = syntheticMbid(3);
	const privateRecording = syntheticMbid(11);
	const unmatchedRecording = syntheticMbid(12);
	const unknownRecording = syntheticMbid(13);
	const names = new Map([
		[seed, 'Seed Artist'], [firstArtist, 'Zulu Artist'],
		[publicLeader, 'Public Lead'], [privateArtist, 'Private-only Artist']
	]);
	const catalogue = [recording(firstRecording), recording(secondRecording)];
	const relationships = [
		...normalizedRelationships,
		...catalogue.flatMap(({ id }) => [relationship(publicLeader, id), relationship(unknownArtist, id)])
	];
	const ineligibleRecordings = [privateRecording, unmatchedRecording, unknownRecording];
	const addedEvidence = ineligibleRecordings.flatMap((id) => [
		relationship(seed, id), relationship(privateArtist, id), relationship(firstArtist, id)
	]);
	const expected = [{ id: publicLeader, name: 'Public Lead' }, { id: firstArtist, name: 'Zulu Artist' }];
	expect(selectRelatedArtists(seed, catalogue, names, relationships)).toEqual(expected);
	expect(selectRelatedArtists(seed, [
		...catalogue,
		recording(privateRecording, { user_id: 'fixture-owner' }),
		recording(unmatchedRecording, { recording_mbids: [firstRecording] })
	], names, [...relationships, ...addedEvidence])).toEqual(expected);
});

test('renders the same empty shelf with missing or ineligible public evidence', () => {
	const names = new Map([[seed, 'Seed Artist'], [firstArtist, 'Fixture Artist']]);
	const catalogue = [recording(firstRecording)];
	const empty = render(RelatedArtistsShelf, { props: { artists: [] } }).body;
	expect(empty).toContain('No related artists are available yet.');
	expect(empty).not.toContain('<ul>');

	for (const artists of [
		selectRelatedArtists(seed, catalogue, names, []),
		selectRelatedArtists(seed, [], names, normalizedRelationships),
		selectRelatedArtists(seed, catalogue, new Map([[firstArtist, 'Fixture Artist']]), normalizedRelationships),
		selectRelatedArtists(seed, catalogue, new Map([[seed, 'Seed Artist']]), normalizedRelationships),
		selectRelatedArtists(seed, [recording(firstRecording, { user_id: 'fixture-owner' })], names, normalizedRelationships)
	]) {
		expect(artists).toEqual([]);
		expect(render(RelatedArtistsShelf, { props: { artists } }).body).toEqual(empty);
	}
});
