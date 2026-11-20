import { describe, expect, it, vi } from 'vitest';
import { loadRecentPlays, recentPlayTitles } from './recent-plays.js';

const play = { play_id: 'Play_A', track_id: 'Job_A', played_at: 1_700_000_000, stream_seconds: 0 };

function client(payload: unknown) {
	return { get: vi.fn(async (_path: string) => payload) };
}

describe('loadRecentPlays', () => {
	it('keeps the server order, repeated tracks and exact ID case', async () => {
		const api = client({ items: [play, { ...play, play_id: 'Play_B' }, { ...play, track_id: 'job_a' }] });
		const result = await loadRecentPlays(api);
		expect(api.get).toHaveBeenCalledExactlyOnceWith('/v1/me/history');
		expect(result.map((row) => row.key)).toEqual(['Job_A:Play_A', 'Job_A:Play_B', 'job_a:Play_A']);
		expect(result[0].playedAt.toISOString()).toBe('2023-11-14T22:13:20.000Z');
	});

	it('accepts empty history without fabricating a play', async () => {
		await expect(loadRecentPlays(client({ items: [] }))).resolves.toEqual([]);
	});

	it.each([
		null, { items: [null] }, { items: [play, play] },
		{ items: Array.from({ length: 51 }, (_, i) => ({ ...play, play_id: `play_${i}` })) },
		{ items: [{ ...play, track_id: 'job/path' }] },
		{ items: [{ ...play, played_at: 253_402_300_800 }] }
	])('refuses malformed or oversized history', async (payload) => {
		await expect(loadRecentPlays(client(payload))).rejects.toThrow('Invalid listening history');
	});
});

it('labels exact job and recording IDs but does not guess for ambiguous jobs', () => {
	const titles = recentPlayTitles([
		{ id: 'recording_a', occurrence_key: 'Job_A:0', title: 'First' },
		{ id: 'recording_b', occurrence_key: 'Job_B:0', title: 'Second' },
		{ id: 'recording_c', occurrence_key: 'Job_B:1', title: 'Third' }
	]);
	expect(titles.get('Job_A')).toBe('First');
	expect(titles.has('job_a')).toBe(false);
	expect(titles.get('recording_b')).toBe('Second');
	expect(titles.has('Job_B')).toBe(false);
});
