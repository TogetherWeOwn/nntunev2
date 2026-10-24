import { describe, expect, it } from 'vitest';
import { parseTimestampParam } from './shareTimestamp.js';

/**
 * PROTECTED GOTO acceptance: the paired Builder changes production code,
 * not this file or tests/player-goto-time.spec.ts.
 *
 * These are GREEN grammar controls over the existing production parser,
 * not a new parser or a test-only validator. Assertion-RED form outcomes,
 * including independent finite/duration guards, live in the browser spec.
 * Keep timestamp grammar shared with inbound links; do not invent a
 * second parser for the form. No new production export is required.
 */
describe('GOTO shared timestamp grammar', () => {
	it.each([
		['GOTO-U01', '0', 0],
		['GOTO-U02', '90', 90],
		['GOTO-U03', '90.5', 90.5],
		['GOTO-U04', '007', 7],
		['GOTO-U05', ' 1:30 ', 90],
		['GOTO-U06', '0:59', 59],
		['GOTO-U07', '60:00', 3600],
		['GOTO-U08', '1:02:03', 3723],
		['GOTO-U09', '0:00:00', 0],
		['GOTO-U10', '1:59:59', 7199]
	])('%s parses %j into exact seconds', (_id, raw, seconds) => {
		expect(parseTimestampParam(raw)).toBe(seconds);
	});

	it.each([
		['GOTO-U11', ''],
		['GOTO-U12', '   '],
		['GOTO-U13', 'soon'],
		['GOTO-U14', '-1'],
		['GOTO-U15', 'NaN'],
		['GOTO-U16', 'Infinity'],
		['GOTO-U17', '1e2'],
		['GOTO-U18', '0x10'],
		['GOTO-U19', '1:60'],
		['GOTO-U20', '1:60:00'],
		['GOTO-U21', '1::02'],
		['GOTO-U22', '1:2:3:4'],
		['GOTO-U23', '1.5:30'],
		['GOTO-U24', '1:30.5'],
		['GOTO-U25', '9'.repeat(309)]
	])('%s rejects %j without a second grammar', (_id, raw) => {
		expect(parseTimestampParam(raw)).toBeNull();
	});
});
