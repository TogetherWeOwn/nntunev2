import { describe, expect, it } from 'vitest';
import * as format from './format';

/**
 * TRACK-TIME independent acceptance handoff (test-only; do not merge RED alone).
 * Protected paths: src/lib/track-time.test.ts and tests/track-time.spec.ts in apps/web.
 * Builder contract: export deriveTrackTime(currentTime: number, duration: number)
 * from src/lib/format.ts, returning the display strings below. Reuse formatDuration
 * after finite-number guards. Unknown duration => duration/remaining null, not zero.
 * Fractions are floored AFTER clamping/subtraction (e.g. 60.9 - 0.1 => 1:00).
 * Unknown elapsed is normalized to zero; finite elapsed clamps to [0, duration]
 * when duration is known. Negative duration is invalid, finite zero is known.
 * No playback rate input: these are media seconds, not wall-clock seconds.
 *
 * Run from repo root:
 *   pnpm --dir apps/web exec vitest run src/lib/track-time.test.ts
 *   CI=1 pnpm --dir apps/web exec playwright test tests/track-time.spec.ts --workers=1
 *   pnpm --dir apps/web check
 * Existing player regression commands are recorded in track-time.spec.ts.
 * The optional namespace lookup deliberately makes a missing new export an
 * assertion mismatch (undefined vs literal display), not an import/type error.
 * Builder implements production only; return incorrect protected tests to author.
 * Verified baseline 9b034604bad8c489ee71effa7544132fe5017dfe:
 * unit exit 1, 18 assertion-RED / 1 formatter control PASS. TT-U01 expected
 * {elapsed:'1:01', remaining:'2:58', duration:'4:00'}, received undefined.
 * Full web units with --maxWorkers=2: 18 intentional RED / 1656 PASS.
 * Default workers also hit 5s billing/page.test.ts and billing/page.trial-deadline.test.ts
 * timeouts; bounded workers did not. These are reported, not waived or modified.
 * Browser: 21 feature assertion-RED / 1 real-player fixture control PASS;
 * 63 existing mocked browser/shortcut regressions PASS. Feature RED is the
 * missing visible summary/choice, not fixture setup, decoder or import failure.
 * Web check and standalone browser-spec strict typecheck: PASS.
 * Branch handoff: test/track-time-acceptance; Builder must make one combined
 * green PR, not merge the RED tests alone. No production stubs were added.
 */
type TrackTime = { elapsed: string; remaining: string | null; duration: string | null };
const deriveTrackTime = (format as unknown as {
	deriveTrackTime?: (currentTime: number, duration: number) => TrackTime;
}).deriveTrackTime;

describe('TRACK-TIME finite media-time derivation', () => {
	const cases: [string, number, number, TrackTime][] = [
		['TT-U01 ordinary fractional elapsed', 61.9, 240, { elapsed: '1:01', remaining: '2:58', duration: '4:00' }],
		['TT-U02 hour-long track', 3661.9, 7322, { elapsed: '1:01:01', remaining: '1:01:00', duration: '2:02:02' }],
		['TT-U03 below hour boundary', 3599.9, 3600, { elapsed: '59:59', remaining: '0:00', duration: '1:00:00' }],
		['TT-U04 at hour boundary', 3600, 7200, { elapsed: '1:00:00', remaining: '1:00:00', duration: '2:00:00' }],
		['TT-U05 negative elapsed clamps', -20.5, 240, { elapsed: '0:00', remaining: '4:00', duration: '4:00' }],
		['TT-U06 overrun clamps', 241, 240, { elapsed: '4:00', remaining: '0:00', duration: '4:00' }],
		['TT-U07 real zero duration', 20, 0, { elapsed: '0:00', remaining: '0:00', duration: '0:00' }],
		['TT-U08 unknown NaN duration', 61.9, NaN, { elapsed: '1:01', remaining: null, duration: null }],
		['TT-U09 infinite duration', 61, Infinity, { elapsed: '1:01', remaining: null, duration: null }],
		['TT-U10 negative infinite duration', 61, -Infinity, { elapsed: '1:01', remaining: null, duration: null }],
		['TT-U11 negative duration', 61, -1, { elapsed: '1:01', remaining: null, duration: null }],
		['TT-U12 NaN elapsed', NaN, 240, { elapsed: '0:00', remaining: '4:00', duration: '4:00' }],
		['TT-U13 infinite elapsed', Infinity, 240, { elapsed: '0:00', remaining: '4:00', duration: '4:00' }],
		['TT-U14 negative infinite elapsed', -Infinity, 240, { elapsed: '0:00', remaining: '4:00', duration: '4:00' }],
		['TT-U15 both unknown', NaN, NaN, { elapsed: '0:00', remaining: null, duration: null }],
		['TT-U16 floor after subtracting fractions', 0.1, 60.9, { elapsed: '0:00', remaining: '1:00', duration: '1:00' }],
		['TT-U17 fractional overrun', 60.95, 60.9, { elapsed: '1:00', remaining: '0:00', duration: '1:00' }],
		['TT-U18 fractional finite duration', 0, 0.9, { elapsed: '0:00', remaining: '0:00', duration: '0:00' }]
	];

	it.each(cases)('%s', (_id, currentTime, duration, expected) => {
		expect(deriveTrackTime?.(currentTime, duration)).toEqual(expected);
	});

	it('TT-U19 existing formatter remains the hour-capable formatting contract', () => {
		expect(format.formatDuration(61.9)).toBe('1:01');
		expect(format.formatDuration(3661.9)).toBe('1:01:01');
	});
});
