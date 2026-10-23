import { describe, expect, it } from 'vitest';
import {
	buildShareQuery,
	parseTimestampParam,
	resolveShareTarget
} from './shareTimestamp.js';

/**
 * Test-Author owned RED acceptance for share links with timestamps. The
 * Builder implements `shareTimestamp.ts` to satisfy this file and must not
 * edit it.
 *
 * LOCKED CONTRACT (`apps/web/src/lib/player/shareTimestamp.ts`):
 * - `parseTimestampParam(raw: string | null | undefined): number | null`
 *   parses the `t` query value into seconds: plain seconds ('90', '90.5'),
 *   'm:ss' ('1:30'), or 'h:mm:ss' ('1:02:03'). Surrounding whitespace is
 *   trimmed. Returns null for null/empty input, non-numeric input, negative
 *   values, non-finite values, malformed clock forms (wrong part counts,
 *   empty parts, non-digit parts, seconds/minutes >= 60 in clock forms).
 * - `resolveShareTarget({ track, t }, catalogueIds)` returns
 *   `{ trackId, seconds }` when `track` names a catalogue id AND `t` parses;
 *   otherwise null (unknown/missing track, or unparsable `t`, never partial).
 * - `buildShareQuery(trackId, seconds)`: renders `track=<enc>&t=<floor>`
 *   with `encodeURIComponent` on the id and `Math.floor` on finite
 *   non-negative seconds. Throws `RangeError` for empty track ids or
 *   non-finite/negative seconds.
 */

const CATALOGUE = ['demo-track', 'second-signal', 'third-light'];

describe('timestamp param parsing', () => {
	it.each([
		['90', 90],
		['0', 0],
		['90.5', 90.5],
		['007', 7]
	])('parses plain seconds %s as %s', (input, expected) => {
		expect(parseTimestampParam(input)).toBe(expected);
	});

	it.each([
		['1:30', 90],
		['01:30', 90],
		['10:00', 600],
		['0:05', 5]
	])('parses clock form %s as %s seconds', (input, expected) => {
		expect(parseTimestampParam(input)).toBe(expected);
	});

	it.each([
		['1:02:03', 3723],
		['0:00:07', 7],
		['2:00:00', 7200]
	])('parses long clock form %s as %s seconds', (input, expected) => {
		expect(parseTimestampParam(input)).toBe(expected);
	});

	it('trims surrounding whitespace', () => {
		expect(parseTimestampParam('  90  ')).toBe(90);
		expect(parseTimestampParam(' 1:30 ')).toBe(90);
	});

	it.each([
		[null],
		[undefined],
		[''],
		['   '],
		['abc'],
		['-5'],
		['-1:30'],
		['NaN'],
		['Infinity'],
		['1:99'],
		['1:60'],
		['1:2:3:4'],
		['1:'],
		[':30'],
		['1::30'],
		['1:3x'],
		['1.5:30'],
		['01:02:99']
	])('rejects %s as null', (input) => {
		expect(parseTimestampParam(input)).toBeNull();
	});
});

describe('share target resolution', () => {
	it('resolves a known track with a plain-seconds timestamp', () => {
		expect(resolveShareTarget({ track: 'second-signal', t: '90' }, CATALOGUE)).toEqual({
			trackId: 'second-signal',
			seconds: 90
		});
	});

	it('resolves clock-form timestamps', () => {
		expect(resolveShareTarget({ track: 'third-light', t: '1:30' }, CATALOGUE)).toEqual({
			trackId: 'third-light',
			seconds: 90
		});
	});

	it.each([[null], [''], ['unknown-track'], [' Demo-Track ']])(
		'returns null for unresolvable track %s even with a valid t',
		(track) => {
			expect(resolveShareTarget({ track, t: '90' }, CATALOGUE)).toBeNull();
		}
	);

	it.each([[null], [''], ['soon'], ['-3']])(
		'returns null for unparsable t %s even with a valid track',
		(t) => {
			expect(resolveShareTarget({ track: 'demo-track', t }, CATALOGUE)).toBeNull();
		}
	);

	it('returns null against an empty catalogue', () => {
		expect(resolveShareTarget({ track: 'demo-track', t: '90' }, [])).toBeNull();
	});
});

describe('share query building', () => {
	it('renders track and floored seconds', () => {
		expect(buildShareQuery('demo-track', 90)).toBe('track=demo-track&t=90');
		expect(buildShareQuery('second-signal', 90.9)).toBe('track=second-signal&t=90');
		expect(buildShareQuery('demo-track', 0)).toBe('track=demo-track&t=0');
	});

	it('encodes track ids', () => {
		expect(buildShareQuery('a b&c', 5)).toBe('track=a%20b%26c&t=5');
	});

	it.each([[''], ['  ']])('throws RangeError for blank track id %s', (trackId) => {
		expect(() => buildShareQuery(trackId, 10)).toThrow(RangeError);
	});

	it.each([[Number.NaN], [Number.POSITIVE_INFINITY], [-1]])(
		'throws RangeError for seconds %s',
		(seconds) => {
			expect(() => buildShareQuery('demo-track', seconds)).toThrow(RangeError);
		}
	);

	it('round-trips: built queries resolve back to the same target', () => {
		const query = buildShareQuery('third-light', 125.7);
		const params = new URLSearchParams(query);

		expect(
			resolveShareTarget(
				{ track: params.get('track'), t: params.get('t') },
				['demo-track', 'second-signal', 'third-light']
			)
		).toEqual({ trackId: 'third-light', seconds: 125 });
	});
});
