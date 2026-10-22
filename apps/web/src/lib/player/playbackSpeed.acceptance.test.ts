import { describe, expect, it } from 'vitest';
import {
	clampPlaybackSpeed,
	createPlaybackSpeed,
	DEFAULT_PLAYBACK_SPEED,
	MAX_PLAYBACK_SPEED,
	MIN_PLAYBACK_SPEED,
	parsePlaybackSpeed,
	PLAYBACK_SPEED_STORAGE_KEY
} from './playbackSpeed.js';

/**
 * Test-Author owned RED acceptance for persisted playback speed. The Builder
 * implements `playbackSpeed.ts` to satisfy this file and must not edit it.
 *
 * LOCKED CONTRACT (`apps/web/src/lib/player/playbackSpeed.ts`):
 * - `MIN_PLAYBACK_SPEED = 0.5`, `MAX_PLAYBACK_SPEED = 2`,
 *   `DEFAULT_PLAYBACK_SPEED = 1`,
 *   `PLAYBACK_SPEED_STORAGE_KEY = 'nntune:playback-speed:v1'`.
 * - `clampPlaybackSpeed(rate)`: finite rates clamp into [0.5, 2];
 *   non-finite rates (NaN, +/-Infinity) become 1.
 * - `parsePlaybackSpeed(raw: string | null)`: parses a stored value;
 *   null/blank/unparseable input becomes 1 (blank is garbage, not 0:
 *   `Number('')` is 0, so trim-and-reject blanks before parsing);
 *   out-of-range numerics fall back through the clamp ('3' becomes 2).
 * - `createPlaybackSpeed({ storage, audio, storageKey? })` where `storage`
 *   is `Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>` and `audio` is
 *   `() => { playbackRate: number } | null | undefined`. Reads the persisted
 *   rate once at construction (fail-soft: storage errors become the default,
 *   never throw). `storageKey` defaults to `PLAYBACK_SPEED_STORAGE_KEY`.
 * - `speed.current()`: the effective rate (persisted-or-default, then sets).
 * - `speed.set(rate)`: clamps, applies to the current audio element when one
 *   exists, persists `String(effective)` (fail-soft: a throwing store still
 *   applies to audio, never throws), returns the effective rate.
 * - `speed.apply()`: re-applies `current()` to the current audio element
 *   (for element swaps), returns the effective rate.
 */

type FakeAudio = { playbackRate: number };

function fakeStorage(initial: Record<string, string> = {}) {
	const map = new Map(Object.entries(initial));
	return {
		getItem: (key: string) => map.get(key) ?? null,
		setItem: (key: string, value: string) => void map.set(key, value),
		removeItem: (key: string) => void map.delete(key),
		raw: map
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

describe('playback speed bounds', () => {
	it('pins the 0.5–2x range with a 1x default', () => {
		expect(MIN_PLAYBACK_SPEED).toBe(0.5);
		expect(MAX_PLAYBACK_SPEED).toBe(2);
		expect(DEFAULT_PLAYBACK_SPEED).toBe(1);
		expect(PLAYBACK_SPEED_STORAGE_KEY).toBe('nntune:playback-speed:v1');
	});

	it.each([
		[0.5, 0.5],
		[1, 1],
		[1.5, 1.5],
		[2, 2],
		[0.1, 0.5],
		[0, 0.5],
		[-3, 0.5],
		[2.0001, 2],
		[3, 2]
	])('clamps %s to %s', (input, expected) => {
		expect(clampPlaybackSpeed(input)).toBe(expected);
	});

	it.each([[Number.NaN], [Number.POSITIVE_INFINITY], [Number.NEGATIVE_INFINITY]])(
		'maps non-finite %s to the default',
		(input) => {
			expect(clampPlaybackSpeed(input)).toBe(DEFAULT_PLAYBACK_SPEED);
		}
	);

	it.each([
		['1.5', 1.5],
		['0.5', 0.5],
		['2', 2],
		['3', 2],
		['0', 0.5],
		[null, 1],
		['', 1],
		['fast', 1],
		['NaN', 1],
		['Infinity', 1]
	])('parses stored %s as %s', (input, expected) => {
		expect(parsePlaybackSpeed(input)).toBe(expected);
	});
});

describe('playback speed persistence round-trip', () => {
	it('set applies to the audio element and persists the effective rate', () => {
		const storage = fakeStorage();
		const audio: FakeAudio = { playbackRate: 1 };
		const speed = createPlaybackSpeed({ storage, audio: () => audio });

		expect(speed.set(1.5)).toBe(1.5);
		expect(audio.playbackRate).toBe(1.5);
		expect(storage.getItem(PLAYBACK_SPEED_STORAGE_KEY)).toBe('1.5');
		expect(speed.current()).toBe(1.5);
	});

	it('set clamps before applying and persisting', () => {
		const storage = fakeStorage();
		const audio: FakeAudio = { playbackRate: 1 };
		const speed = createPlaybackSpeed({ storage, audio: () => audio });

		expect(speed.set(7)).toBe(2);
		expect(audio.playbackRate).toBe(2);
		expect(storage.getItem(PLAYBACK_SPEED_STORAGE_KEY)).toBe('2');
	});

	it('a new instance over the same store reads the persisted rate (reload)', () => {
		const storage = fakeStorage();
		const firstAudio: FakeAudio = { playbackRate: 1 };
		createPlaybackSpeed({ storage, audio: () => firstAudio }).set(1.25);

		const secondAudio: FakeAudio = { playbackRate: 1 };
		const reloaded = createPlaybackSpeed({ storage, audio: () => secondAudio });
		expect(reloaded.current()).toBe(1.25);
		expect(reloaded.apply()).toBe(1.25);
		expect(secondAudio.playbackRate).toBe(1.25);
	});

	it('an invalid persisted value falls back to the default', () => {
		const storage = fakeStorage({ [PLAYBACK_SPEED_STORAGE_KEY]: 'ludicrous' });
		const audio: FakeAudio = { playbackRate: 1 };

		expect(createPlaybackSpeed({ storage, audio: () => audio }).current()).toBe(1);
	});

	it('honours a custom storage key', () => {
		const storage = fakeStorage();
		const audio: FakeAudio = { playbackRate: 1 };
		const speed = createPlaybackSpeed({ storage, audio: () => audio, storageKey: 'custom:key' });

		speed.set(1.75);
		expect(storage.getItem('custom:key')).toBe('1.75');
		expect(storage.getItem(PLAYBACK_SPEED_STORAGE_KEY)).toBeNull();
	});
});

describe('playback speed failure containment', () => {
	it('a throwing store degrades to the default without throwing', () => {
		const audio: FakeAudio = { playbackRate: 1 };

		const speed = createPlaybackSpeed({ storage: throwingStorage(), audio: () => audio });
		expect(speed.current()).toBe(DEFAULT_PLAYBACK_SPEED);
	});

	it('set with a throwing store still applies to audio and never throws', () => {
		const audio: FakeAudio = { playbackRate: 1 };
		const speed = createPlaybackSpeed({ storage: throwingStorage(), audio: () => audio });

		expect(speed.set(1.5)).toBe(1.5);
		expect(audio.playbackRate).toBe(1.5);
		expect(speed.current()).toBe(1.5);
	});

	it('set without an audio element still persists and never throws', () => {
		const storage = fakeStorage();
		const speed = createPlaybackSpeed({ storage, audio: () => null });

		expect(speed.set(1.5)).toBe(1.5);
		expect(storage.getItem(PLAYBACK_SPEED_STORAGE_KEY)).toBe('1.5');
	});

	it('apply pushes the current rate onto a swapped-in element', () => {
		const storage = fakeStorage();
		let element: FakeAudio | null = { playbackRate: 1 };
		const speed = createPlaybackSpeed({ storage, audio: () => element });
		speed.set(1.5);

		element = { playbackRate: 1 };
		expect(speed.apply()).toBe(1.5);
		expect(element.playbackRate).toBe(1.5);
	});

	it('apply without an audio element returns the rate and never throws', () => {
		const storage = fakeStorage();
		const speed = createPlaybackSpeed({ storage, audio: () => undefined });

		expect(speed.apply()).toBe(1);
	});
});
