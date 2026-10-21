import { describe, expect, it } from 'vitest';
import { getPlan, type Plan } from './plans';
import { evaluateUsage, remainingStreamMinutes } from './usage';

const plus = getPlan('plus')!;

/**
 * Synthetic caps chosen so every boundary is an exact integer:
 * stream cap = 10h = 36000s (soft at 28800s), concurrent 5,
 * library 100 tracks, ingest 10/day.
 */
const exact = {
	id: 'test-exact',
	name: 'Test',
	priceCents: 0,
	streamHoursMonth: 10,
	maxConcurrentStreams: 5,
	maxLibraryTracks: 100,
	ingestJobsDay: 10,
	cachePriority: 'standard',
	cacheBytesBudget: 21_474_836_480,
	cacheRetentionDays: 30,
	blurb: ''
} satisfies Plan;

const STREAM_CAP = 10 * 3600;
const SOFT_STREAM = Math.round(STREAM_CAP * 0.8);

function lowUsage() {
	return {
		streamSecondsMonth: 0,
		concurrentNow: 0,
		libraryTracks: 0,
		ingestToday: 0
	};
}

describe('evaluateUsage', () => {
	it('is ok well under every cap', () => {
		const s = evaluateUsage(plus, {
			streamSecondsMonth: 10 * 3600,
			concurrentNow: 0,
			libraryTracks: 100,
			ingestToday: 1
		});
		expect(s.state).toBe('ok');
		expect(s.detail).toBe('');
	});

	it('nudges softly at 80%+ of stream hours', () => {
		const s = evaluateUsage(plus, {
			streamSecondsMonth: 85 * 3600,
			concurrentNow: 0,
			libraryTracks: 100,
			ingestToday: 1
		});
		expect(s.state).toBe('soft');
		expect(s.upgradeTo).toBe('pro-5x');
		expect(s.detail).toContain('degrades');
	});

	it('degrades (never bills) past the monthly cap', () => {
		const s = evaluateUsage(plus, {
			streamSecondsMonth: 100 * 3600 + 1,
			concurrentNow: 0,
			libraryTracks: 100,
			ingestToday: 1
		});
		expect(s.state).toBe('hard');
		expect(s.streamHardCapped).toBe(true);
		expect(s.detail).toContain('lower bitrate');
		expect(s.detail).toContain('No surprise bills');
	});

	it('blocks concurrent streams at the cap with an upgrade hint', () => {
		const s = evaluateUsage(plus, {
			streamSecondsMonth: 0,
			concurrentNow: 2,
			libraryTracks: 100,
			ingestToday: 1
		});
		expect(s.state).toBe('hard');
		expect(s.concurrentBlocked).toBe(true);
		expect(s.detail).toContain('4');
	});

	it('blocks ingest at the daily cap', () => {
		const s = evaluateUsage(plus, {
			streamSecondsMonth: 0,
			concurrentNow: 0,
			libraryTracks: 100,
			ingestToday: 25
		});
		expect(s.state).toBe('hard');
		expect(s.ingestBlocked).toBe(true);
	});

	it('names the real dimension when the library drives the soft state', () => {
		const s = evaluateUsage(plus, {
			streamSecondsMonth: 10 * 3600,
			concurrentNow: 0,
			libraryTracks: 24000,
			ingestToday: 1
		});
		expect(s.state).toBe('soft');
		expect(s.headline).toContain('96%');
		expect(s.headline).toContain('Library');
	});

	it('names the real dimension when ingest drives the soft state', () => {
		const s = evaluateUsage(plus, {
			streamSecondsMonth: 0,
			concurrentNow: 0,
			libraryTracks: 100,
			ingestToday: 24
		});
		expect(s.state).toBe('soft');
		expect(s.headline).toContain('96%');
		expect(s.headline).toContain('Ingest');
	});

	it('reports stream hours when streaming drives the soft state', () => {
		const s = evaluateUsage(plus, {
			streamSecondsMonth: 85 * 3600,
			concurrentNow: 0,
			libraryTracks: 100,
			ingestToday: 1
		});
		expect(s.state).toBe('soft');
		expect(s.headline).toContain('85%');
	});

	it('has no upgrade left at the top tier', () => {
		const top = getPlan('pro-20x')!;
		const s = evaluateUsage(top, {
			streamSecondsMonth: 2001 * 3600,
			concurrentNow: 0,
			libraryTracks: 0,
			ingestToday: 0
		});
		expect(s.state).toBe('hard');
		expect(s.upgradeTo).toBeNull();
	});

	it.each([
		{
			name: 'stream',
			below: { ...lowUsage(), streamSecondsMonth: SOFT_STREAM - 1 },
			at: { ...lowUsage(), streamSecondsMonth: SOFT_STREAM }
		},
		{
			name: 'concurrent',
			below: { ...lowUsage(), concurrentNow: 3 },
			at: { ...lowUsage(), concurrentNow: 4 }
		},
		{
			name: 'library',
			below: { ...lowUsage(), libraryTracks: 79 },
			at: { ...lowUsage(), libraryTracks: 80 }
		},
		{
			name: 'ingest',
			below: { ...lowUsage(), ingestToday: 7 },
			at: { ...lowUsage(), ingestToday: 8 }
		}
	])('soft threshold for $name: just below is ok, at 80% is soft', ({ below, at }) => {
		const okState = evaluateUsage(exact, below);
		expect(okState.state).toBe('ok');
		expect(okState.streamHardCapped).toBe(false);
		expect(okState.concurrentBlocked).toBe(false);
		expect(okState.libraryBlocked).toBe(false);
		expect(okState.ingestBlocked).toBe(false);

		const softState = evaluateUsage(exact, at);
		expect(softState.state).toBe('soft');
		expect(softState.streamHardCapped).toBe(false);
		expect(softState.concurrentBlocked).toBe(false);
		expect(softState.libraryBlocked).toBe(false);
		expect(softState.ingestBlocked).toBe(false);
	});

	it.each([
		{
			name: 'stream',
			below: { ...lowUsage(), streamSecondsMonth: STREAM_CAP - 1 },
			at: { ...lowUsage(), streamSecondsMonth: STREAM_CAP },
			flag: 'streamHardCapped' as const
		},
		{
			name: 'concurrent',
			below: { ...lowUsage(), concurrentNow: 4 },
			at: { ...lowUsage(), concurrentNow: 5 },
			flag: 'concurrentBlocked' as const
		},
		{
			name: 'library',
			below: { ...lowUsage(), libraryTracks: 99 },
			at: { ...lowUsage(), libraryTracks: 100 },
			flag: 'libraryBlocked' as const
		},
		{
			name: 'ingest',
			below: { ...lowUsage(), ingestToday: 9 },
			at: { ...lowUsage(), ingestToday: 10 },
			flag: 'ingestBlocked' as const
		}
	])('hard cap for $name: cap-1 is not blocked, at cap is hard', ({ below, at, flag }) => {
		const near = evaluateUsage(exact, below);
		expect(near[flag]).toBe(false);
		expect(near.state).toBe('soft');

		const capped = evaluateUsage(exact, at);
		expect(capped[flag]).toBe(true);
		expect(capped.state).toBe('hard');
	});

	it('prefers the stream headline when every hard cap fires, keeping all flags truthful', () => {
		const s = evaluateUsage(exact, {
			streamSecondsMonth: STREAM_CAP,
			concurrentNow: 5,
			libraryTracks: 100,
			ingestToday: 10
		});
		expect(s.state).toBe('hard');
		expect(s.streamHardCapped).toBe(true);
		expect(s.concurrentBlocked).toBe(true);
		expect(s.libraryBlocked).toBe(true);
		expect(s.ingestBlocked).toBe(true);
		expect(s.headline).toContain('streaming hours');
	});

	it('prefers concurrency over ingest/library when stream is clear, keeping all flags truthful', () => {
		const s = evaluateUsage(exact, {
			streamSecondsMonth: 0,
			concurrentNow: 5,
			libraryTracks: 100,
			ingestToday: 10
		});
		expect(s.state).toBe('hard');
		expect(s.streamHardCapped).toBe(false);
		expect(s.concurrentBlocked).toBe(true);
		expect(s.libraryBlocked).toBe(true);
		expect(s.ingestBlocked).toBe(true);
		expect(s.headline).toContain('streams are in use');
	});

	it('prefers ingest over library when both fire, keeping both flags truthful', () => {
		const s = evaluateUsage(exact, {
			streamSecondsMonth: 0,
			concurrentNow: 0,
			libraryTracks: 100,
			ingestToday: 10
		});
		expect(s.state).toBe('hard');
		expect(s.streamHardCapped).toBe(false);
		expect(s.concurrentBlocked).toBe(false);
		expect(s.libraryBlocked).toBe(true);
		expect(s.ingestBlocked).toBe(true);
		expect(s.headline).toContain('ingest');
	});

	it('names the dimension with the largest soft fraction', () => {
		const streamWins = evaluateUsage(exact, {
			streamSecondsMonth: Math.round(STREAM_CAP * 0.85),
			concurrentNow: 0,
			libraryTracks: 82,
			ingestToday: 0
		});
		expect(streamWins.state).toBe('soft');
		expect(streamWins.headline).toContain('streaming hours');

		const libraryWins = evaluateUsage(exact, {
			streamSecondsMonth: Math.round(STREAM_CAP * 0.85),
			concurrentNow: 0,
			libraryTracks: 90,
			ingestToday: 0
		});
		expect(libraryWins.state).toBe('soft');
		expect(libraryWins.headline).toContain('Library');
	});

	it('keeps the first dimension on stable equal-fraction soft ties', () => {
		const allTied = evaluateUsage(exact, {
			streamSecondsMonth: SOFT_STREAM,
			concurrentNow: 4,
			libraryTracks: 80,
			ingestToday: 8
		});
		expect(allTied.state).toBe('soft');
		expect(allTied.headline).toContain('streaming hours');

		const libraryBeforeIngest = evaluateUsage(exact, {
			streamSecondsMonth: 0,
			concurrentNow: 0,
			libraryTracks: 90,
			ingestToday: 9
		});
		expect(libraryBeforeIngest.state).toBe('soft');
		expect(libraryBeforeIngest.headline).toContain('Library');
	});

	it('has no upgrade left at the top tier in soft state either', () => {
		const top = getPlan('pro-20x')!;
		const s = evaluateUsage(top, {
			streamSecondsMonth: 1700 * 3600,
			concurrentNow: 0,
			libraryTracks: 0,
			ingestToday: 0
		});
		expect(s.state).toBe('soft');
		expect(s.upgradeTo).toBeNull();
		expect(s.detail).toBe('Heads up before anything degrades.');
	});
});

describe('remainingStreamMinutes', () => {
	it('floors at zero past the cap', () => {
		expect(remainingStreamMinutes(plus, 0)).toBe(6000);
		expect(remainingStreamMinutes(plus, 100 * 3600 + 5000)).toBe(0);
	});
});
