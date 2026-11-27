import { expect, test } from 'vitest';
import { deriveTrackTime, formatDuration } from './format.js';

test('non-finite or unsafe media times do not produce display coordinates', () => {
	for (const seconds of [NaN, Infinity, -Infinity, -1, Number.MAX_VALUE]) {
		expect(formatDuration(seconds)).toBe('0:00');
		expect(deriveTrackTime(seconds, seconds)).toEqual({
			elapsed: '0:00', remaining: null, duration: null
		});
	}
	expect(deriveTrackTime(70.1, 65.9)).toEqual({
		elapsed: '1:05', remaining: '0:00', duration: '1:05'
	});
});
