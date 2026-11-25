import { render } from 'svelte/server';
import { expect, test } from 'vitest';
import { selectDiscoveryTracks, type DiscoveryCandidate } from '../server/discovery-shelf.js';
import DiscoveryShelf from './DiscoveryShelf.svelte';

const recordingId = (index: number) => `aaaaaaaa-1111-2222-3333-${String(index).padStart(12, '0')}`;

function candidate(index: number, title: string, overrides: Partial<DiscoveryCandidate> = {}): DiscoveryCandidate {
	const id = recordingId(index);
	return {
		user_id: null,
		id,
		release_mbid: 'bbbbbbbb-1111-2222-3333-444444444444',
		recording_mbids: [id],
		track_no: index,
		title,
		artist: 'Fixture Artist',
		...overrides
	};
}

function shelf(candidates: readonly DiscoveryCandidate[]): string {
	return render(DiscoveryShelf, { props: { tracks: selectDiscoveryTracks(candidates) } }).body;
}

test('renders the first five distinct eligible tracks in catalogue order', () => {
	const duplicateId = recordingId(1).toUpperCase();
	const html = shelf([
		candidate(1, 'Private copy', { user_id: 'fixture-owner' }),
		candidate(1, 'Zulu'),
		candidate(1, 'Duplicate recording', { id: duplicateId, recording_mbids: [duplicateId] }),
		candidate(2, 'Echo & Tide', { artist: null }),
		candidate(7, 'Unmatched upload', { recording_mbids: [] }),
		candidate(3, 'Alpha', { artist: ' ' }),
		candidate(4, 'Delta'),
		candidate(5, 'Bravo'),
		candidate(6, 'Charlie')
	]);

	expect([...html.matchAll(/<li>(.*?)<\/li>/g)].map(([, label]) => label)).toEqual([
		'Zulu — Fixture Artist',
		'Echo &amp; Tide',
		'Alpha',
		'Delta — Fixture Artist',
		'Bravo — Fixture Artist'
	]);
});

test('renders an empty shelf for an empty or entirely ineligible catalogue', () => {
	const ineligible = [
		candidate(1, 'Owned track', { user_id: 'fixture-owner' }),
		candidate(2, 'No release match', { release_mbid: null, track_no: null }),
		candidate(3, 'No exact mirror match', { track_no: null }),
		candidate(4, 'No matched recordings', { recording_mbids: [] }),
		candidate(5, 'Job reference', { recording_mbids: [recordingId(6)] }),
		candidate(6, '', { title: null }),
		candidate(7, ' ')
	];
	for (const candidates of [[], ineligible]) {
		const html = shelf(candidates);
		expect(html).toContain('No catalogue tracks are available to discover yet.');
		expect(html).not.toContain('<ul>');
	}
});

test('projects only the recording display metadata', () => {
	const tracks = selectDiscoveryTracks([candidate(1, 'Zulu')]);
	expect(tracks).toEqual([{ id: recordingId(1), title: 'Zulu', artist: 'Fixture Artist' }]);
});
