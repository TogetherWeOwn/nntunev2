import { describe, expect, it, vi } from 'vitest';
import { createPlaybackSpeed } from './playbackSpeed.js';

function setup() {
	let saved = '0.5';
	let decoder = { playbackRate: 1 };
	const storage = {
		getItem: () => saved,
		setItem: vi.fn((_key: string, value: string) => void (saved = value)),
		removeItem: () => {}
	};
	return {
		storage,
		speed: createPlaybackSpeed({ storage, audio: () => decoder }),
		externalSave: () => void (saved = '1.75'),
		attach: (current: typeof decoder) => void (decoder = current)
	};
}

describe('demo playback speed and shared storage', () => {
	it('does not write over a newer preference during restore or on its echo', () => {
		const { speed, storage, externalSave, attach } = setup();
		const decoder = { playbackRate: 1 };
		attach(decoder);
		externalSave();

		expect(speed.apply()).toBe(0.5);
		expect(decoder.playbackRate).toBe(0.5);
		expect(storage.setItem).not.toHaveBeenCalled();
		expect(speed.observeNativeRate(decoder)).toBeUndefined();
		expect(storage.getItem()).toBe('1.75');
	});

	it('adopts a different raw rate even when its persisted bound is unchanged', () => {
		const { speed, storage, externalSave, attach } = setup();
		const decoder = { playbackRate: 1 };
		attach(decoder);
		expect(speed.set(0.5)).toBe(0.5);
		externalSave();

		expect(speed.observeNativeRate(decoder)).toBeUndefined();
		expect(storage.getItem()).toBe('1.75');
		decoder.playbackRate = 0.25;
		expect(speed.observeNativeRate(decoder)).toBe(0.25);
		expect(storage.getItem()).toBe('0.5');

		externalSave();
		expect(speed.observeNativeRate(decoder)).toBeUndefined();
		expect(storage.getItem()).toBe('1.75');
	});

	it.each(['apply', 'observeNativeRate'] as const)(
		'ignores a decoder that retires during %s readback or emits a later event',
		(operation) => {
			const { speed, storage, attach } = setup();
			const current = { playbackRate: 0.5 };
			const read = vi.fn(() => {
				attach(current);
				return 1.25;
			});
			const retired = {
				get playbackRate() {
					return read();
				},
				set playbackRate(_rate: number) {}
			};
			attach(retired);

			if (operation === 'apply') expect(speed.apply()).toBe(0.5);
			else expect(speed.observeNativeRate(retired)).toBeUndefined();
			expect(speed.observeNativeRate(retired)).toBeUndefined();
			expect(read).toHaveBeenCalledOnce();
			expect(storage.setItem).not.toHaveBeenCalled();
		}
	);
});
