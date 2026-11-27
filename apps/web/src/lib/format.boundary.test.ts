import { expect, test } from 'vitest';
import { deriveTrackTime, formatDuration } from './format.js';

test.each([NaN, Infinity, -1, Number.MAX_VALUE])('invalid media time %s shows no duration or remainder', (seconds) => {
	expect(formatDuration(seconds)).toBe('0:00');
	expect(deriveTrackTime(seconds, seconds)).toEqual({ elapsed: '0:00', remaining: null, duration: null });
});
