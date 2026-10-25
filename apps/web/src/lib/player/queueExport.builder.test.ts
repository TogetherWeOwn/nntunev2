import { describe, expect, it } from 'vitest';
import { createQueue, DEMO_TRACKS, formatQueueM3U, formatQueueText } from './queue.js';

describe('queue export boundary', () => {
	it('is read-only even with a frozen queue and frozen catalogue', () => {
		const state = createQueue(DEMO_TRACKS.map((track) => Object.freeze({ ...track })));
		Object.freeze(state.tracks);
		Object.freeze(state);
		const before = JSON.stringify(state);
		expect(formatQueueText(state)).toContain('1. Demo track — Fixture Band');
		expect(formatQueueM3U(state)).toContain('\ndemo-track\n');
		expect(JSON.stringify(state)).toBe(before);
	});

	it('treats Unicode line separators and C1 controls as line breaks, not export markup', () => {
		const state = createQueue([{
			...DEMO_TRACKS[0]!,
			title: `One${String.fromCodePoint(0x2028)}Two${String.fromCodePoint(0x2029)}Three`,
			artist: `A${String.fromCodePoint(0x85)}B`
		}]);
		expect(formatQueueText(state)).toBe('1. One Two Three — A B');
		expect(formatQueueM3U(state)).toBe('#EXTM3U\n#EXTINF:-1,One Two Three — A B\ndemo-track');
	});

	it('uses display fallbacks when runtime metadata is absent', () => {
		const state = createQueue([{ ...DEMO_TRACKS[0]!, title: undefined, artist: null } as unknown as typeof DEMO_TRACKS[number]]);
		expect(formatQueueText(state)).toBe('1. demo-track');
		expect(formatQueueM3U(state)).toBe('#EXTM3U\n#EXTINF:-1,demo-track\ndemo-track');
	});

	it.each(['a\tb', ' track-id ', '', '#EXTINF', 'https://audio.example/track', 'person@example.test'])(
		'refuses unsafe M3U ids instead of changing identity or emitting a URL: %j', (id) => {
			const state = createQueue([{ ...DEMO_TRACKS[0]!, id }]);
			expect(() => formatQueueM3U(state)).toThrow(/track id/i);
		}
	);
});
