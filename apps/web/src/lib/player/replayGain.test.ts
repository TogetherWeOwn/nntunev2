import { describe, expect, it } from 'vitest';
import {
	applyClippingGuard,
	gainDbToLinear,
	LEVELING_STORAGE_KEY,
	loadLevelingEnabled,
	parseGainDb,
	resolveTrackLinearGain,
	saveLevelingEnabled,
	type StorageLike
} from './replayGain.js';

/**
 * Builder-owned edge pins for the leveling core. The paired acceptance suite
 * (`replay-gain-acceptance.test.ts`, never edited here) owns the contract;
 * these pin the corners it leaves open: numeric-string tags, preamp
 * semantics, peak-string handling, the exact full-scale boundary, and
 * throwing storage.
 */

function fakeStorage(initial: Record<string, string> = {}): StorageLike {
	const map = new Map<string, string>(Object.entries(initial));
	return {
		getItem: (key: string): string | null => (map.has(key) ? (map.get(key) as string) : null),
		setItem: (key: string, value: string): void => {
			map.set(key, value);
		}
	};
}

describe('parseGainDb edges', () => {
	it('accepts bare numeric strings and fractional-only tags', () => {
		expect(parseGainDb('-3.25')).toBe(-3.25);
		expect(parseGainDb('.5 dB')).toBe(0.5);
		expect(parseGainDb('0')).toBe(0);
	});

	it('rejects exponents, hex, doubled signs and whitespace-only text', () => {
		for (const bad of ['1e3 dB', '0x10 dB', '++3 dB', '+-3 dB', '   ', '5. dB', 'dB -3']) {
			expect(parseGainDb(bad), `rejected: ${bad}`).toBeNull();
		}
	});

	it('rejects numeric text that overflows to infinity', () => {
		expect(parseGainDb(`${'9'.repeat(400)} dB`)).toBeNull();
	});
});

describe('resolveTrackLinearGain preamp semantics', () => {
	it('falls back to the album tag when the track tag is garbage', () => {
		expect(
			resolveTrackLinearGain(
				{ trackGainDb: 'loud', albumGainDb: '-2.00 dB' },
				{ enabled: true }
			)
		).toBeCloseTo(Math.pow(10, -2 / 20), 10);
	});

	it('adds the preamp to untagged tracks (a global offset, default 0)', () => {
		expect(resolveTrackLinearGain({}, { enabled: true, preampDb: 3 })).toBeCloseTo(
			Math.pow(10, 3 / 20),
			10
		);
	});

	it('treats a non-finite preamp as 0 instead of poisoning the gain', () => {
		for (const preampDb of [Number.NaN, Number.POSITIVE_INFINITY, Number.NEGATIVE_INFINITY]) {
			expect(
				resolveTrackLinearGain({ trackGainDb: '-3.00 dB' }, { enabled: true, preampDb })
			).toBeCloseTo(Math.pow(10, -3 / 20), 10);
		}
		expect(resolveTrackLinearGain({}, { enabled: true, preampDb: Number.NaN })).toBe(1);
	});

	it('uses the album peak when the track peak is invalid', () => {
		expect(
			resolveTrackLinearGain(
				{ trackGainDb: '+10.00 dB', trackPeak: 'loud', albumPeak: 0.9 },
				{ enabled: true }
			)
		).toBeCloseTo(1 / 0.9, 9);
	});
});

describe('applyClippingGuard edges', () => {
	it('passes the exact full-scale boundary through untouched', () => {
		expect(applyClippingGuard(4, 0.25)).toBe(4);
	});

	it('parses numeric-string peaks and rejects boolean peaks', () => {
		expect(applyClippingGuard(gainDbToLinear(10), '0.9')).toBeCloseTo(1 / 0.9, 9);
		const linear = gainDbToLinear(3);
		expect(applyClippingGuard(linear, true)).toBe(linear);
		expect(applyClippingGuard(linear, '  ')).toBe(linear);
	});
});

describe('leveling toggle storage edges', () => {
	it('reads ON and writes silently when storage throws', () => {
		const throwing: StorageLike = {
			getItem: (): string | null => {
				throw new Error('private mode');
			},
			setItem: (): void => {
				throw new Error('private mode');
			}
		};
		expect(loadLevelingEnabled(throwing)).toBe(true);
		expect(() => saveLevelingEnabled(throwing, false)).not.toThrow();
	});

	it('persists under the pinned key with 1/0 values', () => {
		const store = fakeStorage();
		saveLevelingEnabled(store, true);
		expect(store.getItem(LEVELING_STORAGE_KEY)).toBe('1');
		saveLevelingEnabled(store, false);
		expect(store.getItem(LEVELING_STORAGE_KEY)).toBe('0');
	});
});
