import { describe, expect, it } from 'vitest';
import { getPlan, type Plan } from './plans';
import { evaluateUsage, formatResetInstant, remainingStreamMinutes } from './usage';

const plus = getPlan('plus')!;

/**
 * Synthetic cap chosen so every boundary is an exact integer:
 * stream cap = 10h = 36000s (soft at 28800s).
 */
const exact = {
	id: 'test-exact',
	name: 'Test',
	priceCents: 0,
	streamHoursMonth: 10,
	maxConcurrentStreams: 1,
	selectable: true,
	blurb: ''
} satisfies Plan;

const STREAM_CAP = 10 * 3600;
const SOFT_STREAM = Math.round(STREAM_CAP * 0.8);

describe('evaluateUsage', () => {
	it('is ok well under the cap', () => {
		const s = evaluateUsage(plus, { streamSecondsMonth: 10 * 3600 });
		expect(s.state).toBe('ok');
		expect(s.detail).toBe('');
		expect(s.streamHardCapped).toBe(false);
	});

	it('nudges softly at 80%+ of stream hours with the next tier as upgrade', () => {
		const s = evaluateUsage(plus, { streamSecondsMonth: 85 * 3600 });
		expect(s.state).toBe('soft');
		expect(s.upgradeTo).toBe('pro');
		expect(s.headline).toContain('85%');
	});

	it('at the cap, new playback pauses (never bills, never degrades bitrate)', () => {
		const s = evaluateUsage(plus, { streamSecondsMonth: 100 * 3600 + 1 });
		expect(s.state).toBe('hard');
		expect(s.streamHardCapped).toBe(true);
		expect(s.detail).toContain('track already playing finishes');
		expect(s.detail).toContain('No surprise bills');
		expect(s.detail).not.toContain('bitrate');
	});

	it('soft threshold: just below 80% is ok, at 80% is soft', () => {
		const okState = evaluateUsage(exact, { streamSecondsMonth: SOFT_STREAM - 1 });
		expect(okState.state).toBe('ok');
		expect(okState.streamHardCapped).toBe(false);

		const softState = evaluateUsage(exact, { streamSecondsMonth: SOFT_STREAM });
		expect(softState.state).toBe('soft');
		expect(softState.streamHardCapped).toBe(false);
		expect(softState.streamFraction).toBeCloseTo(0.8);
	});

	it('hard cap: cap-1 second is soft, at cap is hard', () => {
		const near = evaluateUsage(exact, { streamSecondsMonth: STREAM_CAP - 1 });
		expect(near.streamHardCapped).toBe(false);
		expect(near.state).toBe('soft');

		const capped = evaluateUsage(exact, { streamSecondsMonth: STREAM_CAP });
		expect(capped.streamHardCapped).toBe(true);
		expect(capped.state).toBe('hard');
	});

	it('a 0-hour plan (reserved free row) is at cap by definition', () => {
		const free = getPlan('free')!;
		const s = evaluateUsage(free, { streamSecondsMonth: 0 });
		expect(s.state).toBe('hard');
		expect(s.streamHardCapped).toBe(true);
	});

	it('has no upgrade left at the top tier, hard or soft', () => {
		const top = getPlan('max')!;
		const hard = evaluateUsage(top, { streamSecondsMonth: 751 * 3600 });
		expect(hard.state).toBe('hard');
		expect(hard.upgradeTo).toBeNull();

		const soft = evaluateUsage(top, { streamSecondsMonth: 700 * 3600 });
		expect(soft.state).toBe('soft');
		expect(soft.upgradeTo).toBeNull();
		expect(soft.detail).toBe('Heads up before you reach the cap.');
	});

	it('fraction matches the ledger fold over the plan cap', () => {
		const s = evaluateUsage(plus, { streamSecondsMonth: 25 * 3600 });
		expect(s.streamFraction).toBeCloseTo(0.25);
		expect(s.headline).toContain('25%');
	});
});

describe('remainingStreamMinutes', () => {
	it('floors at zero past the cap', () => {
		expect(remainingStreamMinutes(plus, 0)).toBe(6000);
		expect(remainingStreamMinutes(plus, 100 * 3600 + 5000)).toBe(0);
	});
});

describe('formatResetInstant', () => {
	it('renders the spec §3.2 reset line in UTC', () => {
		expect(formatResetInstant('2026-12-01T00:00:00Z')).toBe('Resets 1 Dec, 00:00 UTC');
		expect(formatResetInstant('2027-01-01T00:00:00Z')).toBe('Resets 1 Jan, 00:00 UTC');
	});

	it('omits the line for missing or unparseable instants', () => {
		expect(formatResetInstant(null)).toBe('');
		expect(formatResetInstant(undefined)).toBe('');
		expect(formatResetInstant('not-a-date')).toBe('');
	});
});
