import { describe, expect, it } from 'vitest';
import {
	clampPlaybackSpeed,
	createPlaybackSpeed,
	DEFAULT_PLAYBACK_SPEED,
	PLAYBACK_SPEED_STORAGE_KEY,
	parsePlaybackSpeed
} from './playbackSpeed.js';

/**
 * Test-Author owned RED acceptance for rate truth: the speed state, the
 * displayed selection and the persisted preference must agree with the rate
 * the media element actually plays. The Builder implements
 * `playbackSpeed.ts` (and the demo player wiring) to satisfy this file and
 * must not edit it.
 *
 * Sibling `playbackSpeed.acceptance.test.ts` pins the existing export shapes,
 * range, key and no-audio/fail-soft behavior; this file only refines what
 * "effective" means and must stay compatible with every passing assertion
 * there. Stable case IDs: RATETRUTH-U01..U12.
 *
 * LOCKED VERIFICATION CONTRACT (refines `createPlaybackSpeed`):
 * - `set(rate)` clamps the request, writes it to the current element when one
 *   exists, then reads the element back. A finite readback becomes the
 *   current rate (clamped into range) and is persisted, even when the media
 *   rejected or clamped the request: the stored value is the verified actual
 *   rate, never an unapplied request.
 * - When the media write throws, the readback throws, or the readback is
 *   non-finite, `set` keeps the previous current rate, leaves storage
 *   untouched, returns the previous rate and never throws.
 * - `apply()` follows the same rule for a swapped-in element: a valid
 *   readback reconciles current state and persistence to the actual rate; a
 *   throwing setter, throwing getter or non-finite reading retains the
 *   previous rate, leaves storage untouched and never throws.
 * - Without an audio element, `set`/`apply` keep the sibling-pinned behavior
 *   (clamp, persist the effective rate, return it, never throw).
 * - The storage key, schema (`String(rate)`), supported range [0.5, 2] and
 *   1x default are unchanged. Valid stored values (including non-preset 1.3)
 *   are never snapped to presets.
 */

type RateHolder = { playbackRate: number };

function fakeStorage(initial: Record<string, string> = {}) {
	const map = new Map(Object.entries(initial));
	return {
		getItem: (key: string) => map.get(key) ?? null,
		setItem: (key: string, value: string) => void map.set(key, value),
		removeItem: (key: string) => void map.delete(key)
	};
}

function throwingStorage() {
	return {
		getItem: (_key: string): string | null => {
			throw new Error('storage unavailable');
		},
		setItem: (_key: string, _value: string): void => {
			throw new Error('storage unavailable');
		},
		removeItem: (_key: string): void => {
			throw new Error('storage unavailable');
		}
	};
}

/** Media that silently ignores every write: reads always return the old rate. */
function rejectingAudio(rate = 1): RateHolder {
	return Object.defineProperty({} as RateHolder, 'playbackRate', {
		configurable: true,
		enumerable: true,
		get: () => rate,
		set: (_value: number) => {
			// The request is rejected: the actual rate never moves.
		}
	});
}

/** Media that caps the applied rate (platform clamp below the request). */
function clampingAudio(cap: number, initial = 1): RateHolder {
	let backing = initial;
	return Object.defineProperty({} as RateHolder, 'playbackRate', {
		configurable: true,
		enumerable: true,
		get: () => backing,
		set: (value: number) => {
			backing = Math.min(value, cap);
		}
	});
}

/** Media whose setter throws (detached or restricted element). */
function throwingSetterAudio(rate = 1): RateHolder {
	return Object.defineProperty({} as RateHolder, 'playbackRate', {
		configurable: true,
		enumerable: true,
		get: () => rate,
		set: (_value: number) => {
			throw new Error('rate setter unavailable');
		}
	});
}

/** Media that accepts writes but whose getter throws (unverifiable apply). */
function throwingGetterAudio(): RateHolder {
	return Object.defineProperty({} as RateHolder, 'playbackRate', {
		configurable: true,
		enumerable: true,
		get: (): number => {
			throw new Error('rate getter unavailable');
		},
		set: (_value: number) => {
			// Accepted, but no readback can confirm it.
		}
	});
}

/** Media that reports a non-finite reading despite accepting writes. */
function nonfiniteGetterAudio(reading: number): RateHolder {
	return Object.defineProperty({} as RateHolder, 'playbackRate', {
		configurable: true,
		enumerable: true,
		get: () => reading,
		set: (_value: number) => {
			// Accepted, but the native reading is unusable.
		}
	});
}

describe('rate truth: non-preset and boundary parsing', () => {
	it('[RATETRUTH-U01] preserves the non-preset rate 1.3 instead of snapping to a preset', () => {
		expect(parsePlaybackSpeed('1.3')).toBe(1.3);
		expect(clampPlaybackSpeed(1.3)).toBe(1.3);
		expect(parsePlaybackSpeed('1.3')).not.toBe(1.25);
		expect(parsePlaybackSpeed('1.3')).not.toBe(1.5);

		const storage = fakeStorage();
		const audio: RateHolder = { playbackRate: 1 };
		const speed = createPlaybackSpeed({ storage, audio: () => audio });
		expect(speed.set(1.3)).toBe(1.3);
		expect(audio.playbackRate).toBe(1.3);
		expect(storage.getItem(PLAYBACK_SPEED_STORAGE_KEY)).toBe('1.3');

		const reloaded = createPlaybackSpeed({ storage, audio: () => ({ playbackRate: 1 }) });
		expect(reloaded.current()).toBe(1.3);
	});

	it('[RATETRUTH-U02] pins supported boundaries and maps invalids to the default', () => {
		expect(clampPlaybackSpeed(0.5)).toBe(0.5);
		expect(clampPlaybackSpeed(2)).toBe(2);
		expect(parsePlaybackSpeed('0.5')).toBe(0.5);
		expect(parsePlaybackSpeed('2')).toBe(2);
		expect(clampPlaybackSpeed(0.49)).toBe(0.5);
		expect(clampPlaybackSpeed(2.01)).toBe(2);
		expect(parsePlaybackSpeed('3')).toBe(2);
		expect(parsePlaybackSpeed('0')).toBe(0.5);
		for (const raw of [null, '', '  ', 'fast', 'NaN', 'Infinity']) {
			expect(parsePlaybackSpeed(raw)).toBe(DEFAULT_PLAYBACK_SPEED);
		}
		for (const bad of [Number.NaN, Number.POSITIVE_INFINITY, Number.NEGATIVE_INFINITY]) {
			expect(clampPlaybackSpeed(bad)).toBe(DEFAULT_PLAYBACK_SPEED);
		}
		// Boundaries are real rates, never the default by accident.
		expect(parsePlaybackSpeed('0.5')).not.toBe(DEFAULT_PLAYBACK_SPEED);
		expect(parsePlaybackSpeed('2')).not.toBe(DEFAULT_PLAYBACK_SPEED);
	});
});

describe('rate truth: rejected, clamped and throwing media', () => {
	it('[RATETRUTH-U03] set with a throwing media setter keeps the last-known rate and never throws', () => {
		const storage = fakeStorage();
		const audio = throwingSetterAudio(1);
		const speed = createPlaybackSpeed({ storage, audio: () => audio });
		expect(speed.current()).toBe(1);

		expect(() => speed.set(1.5)).not.toThrow();
		expect(speed.current()).toBe(1);
		expect(storage.getItem(PLAYBACK_SPEED_STORAGE_KEY)).toBeNull();
		expect(audio.playbackRate).toBe(1);
	});

	it('[RATETRUTH-U04] set rejected by media reads back the actual rate and never persists the request', () => {
		const storage = fakeStorage();
		const audio = rejectingAudio(1);
		const speed = createPlaybackSpeed({ storage, audio: () => audio });

		expect(speed.set(1.5)).toBe(1);
		expect(speed.current()).toBe(1);
		expect(audio.playbackRate).toBe(1);
		expect(storage.getItem(PLAYBACK_SPEED_STORAGE_KEY)).toBe('1');
		expect(storage.getItem(PLAYBACK_SPEED_STORAGE_KEY)).not.toBe('1.5');
	});

	it('[RATETRUTH-U05] set clamped by media reconciles to the applied rate and persists it', () => {
		const storage = fakeStorage();
		const audio = clampingAudio(1.5);
		const speed = createPlaybackSpeed({ storage, audio: () => audio });

		expect(speed.set(1.7)).toBe(1.5);
		expect(speed.current()).toBe(1.5);
		expect(audio.playbackRate).toBe(1.5);
		expect(storage.getItem(PLAYBACK_SPEED_STORAGE_KEY)).toBe('1.5');
		expect(storage.getItem(PLAYBACK_SPEED_STORAGE_KEY)).not.toBe('1.7');
	});

	it('[RATETRUTH-U06] set with a throwing media getter retains the last-known rate without persisting', () => {
		const storage = fakeStorage();
		const audio = throwingGetterAudio();
		const speed = createPlaybackSpeed({ storage, audio: () => audio });

		expect(() => speed.set(1.5)).not.toThrow();
		expect(speed.current()).toBe(1);
		expect(storage.getItem(PLAYBACK_SPEED_STORAGE_KEY)).toBeNull();
	});

	it('[RATETRUTH-U07] a non-finite native reading never poisons the persisted preference', () => {
		for (const reading of [Number.NaN, Number.POSITIVE_INFINITY, Number.NEGATIVE_INFINITY]) {
			const storage = fakeStorage({ [PLAYBACK_SPEED_STORAGE_KEY]: '1.25' });
			const audio = nonfiniteGetterAudio(reading);
			const speed = createPlaybackSpeed({ storage, audio: () => audio });
			expect(speed.current()).toBe(1.25);

			expect(speed.set(1.5)).toBe(1.25);
			expect(speed.current()).toBe(1.25);
			expect(storage.getItem(PLAYBACK_SPEED_STORAGE_KEY)).toBe('1.25');
		}
	});

	it('[RATETRUTH-U08] apply with a throwing media setter never throws and keeps state', () => {
		const storage = fakeStorage();
		let element: RateHolder = { playbackRate: 1 };
		const speed = createPlaybackSpeed({ storage, audio: () => element });
		expect(speed.set(1.5)).toBe(1.5);

		element = throwingSetterAudio(1);
		expect(() => speed.apply()).not.toThrow();
		expect(speed.current()).toBe(1.5);
		expect(storage.getItem(PLAYBACK_SPEED_STORAGE_KEY)).toBe('1.5');
	});

	it('[RATETRUTH-U09] apply rejected by a swapped-in element reconciles control and storage to actual', () => {
		const storage = fakeStorage();
		let element: RateHolder = { playbackRate: 1 };
		const speed = createPlaybackSpeed({ storage, audio: () => element });
		expect(speed.set(1.5)).toBe(1.5);

		element = rejectingAudio(1);
		expect(speed.apply()).toBe(1);
		expect(speed.current()).toBe(1);
		expect(element.playbackRate).toBe(1);
		expect(storage.getItem(PLAYBACK_SPEED_STORAGE_KEY)).toBe('1');
	});
});

describe('rate truth: storage and no-audio controls', () => {
	it('[RATETRUTH-U10] blocked storage stays usable in memory across sets', () => {
		const audio: RateHolder = { playbackRate: 1 };
		const speed = createPlaybackSpeed({ storage: throwingStorage(), audio: () => audio });
		expect(speed.current()).toBe(DEFAULT_PLAYBACK_SPEED);

		expect(() => speed.set(1.5)).not.toThrow();
		expect(speed.current()).toBe(1.5);
		expect(audio.playbackRate).toBe(1.5);
		expect(() => speed.set(1.25)).not.toThrow();
		expect(speed.current()).toBe(1.25);
		expect(audio.playbackRate).toBe(1.25);
	});

	it('[RATETRUTH-U11] preserves the storage key and schema for valid values', () => {
		expect(PLAYBACK_SPEED_STORAGE_KEY).toBe('nntune:playback-speed:v1');

		const storage = fakeStorage({ [PLAYBACK_SPEED_STORAGE_KEY]: '1.3' });
		const audio: RateHolder = { playbackRate: 1 };
		const speed = createPlaybackSpeed({ storage, audio: () => audio });
		expect(speed.current()).toBe(1.3);
		expect(storage.getItem(PLAYBACK_SPEED_STORAGE_KEY)).toBe('1.3');

		speed.set(1.3);
		expect(storage.getItem(PLAYBACK_SPEED_STORAGE_KEY)).toBe('1.3');
	});

	it('[RATETRUTH-U12] set and apply without an audio element persist and never throw', () => {
		const storage = fakeStorage();
		const speed = createPlaybackSpeed({ storage, audio: () => null });

		expect(speed.set(1.5)).toBe(1.5);
		expect(speed.current()).toBe(1.5);
		expect(storage.getItem(PLAYBACK_SPEED_STORAGE_KEY)).toBe('1.5');
		expect(speed.apply()).toBe(1.5);
	});
});
