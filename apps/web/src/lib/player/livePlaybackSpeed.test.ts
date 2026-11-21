import { get } from 'svelte/store';
import { expect, test, vi } from 'vitest';
import { createLivePlaybackSpeed } from './livePlaybackSpeed.js';

function fixture(seed: string | null = '1.3') {
	let target: { audio: { playbackRate: number }; matches: () => boolean } | undefined;
	const speed = createLivePlaybackSpeed(() => target);
	let stored: string | null = seed;
	const storage = {
		getItem: () => stored,
		setItem: vi.fn((_key: string, rate: string) => { stored = rate; }),
		removeItem: vi.fn()
	};
	speed.restore(storage);
	function attach(accept = (rate: number) => rate) {
		let rate = 1;
		const apply = vi.fn((value: number) => { rate = accept(value); });
		const audio = { get playbackRate() { return rate; }, set playbackRate(value) { apply(value); } };
		const current = { audio, matches: () => target === current };
		target = current;
		speed.attach();
		return { current, audio, apply, native: (next: number) => { rate = next; speed.observe(current); } };
	}
	return { speed, storage, attach, retire: () => { target = undefined; speed.retire(); } };
}

test('a passive attach in a stale tab leaves the newer shared preference untouched', () => {
	const { speed, storage, attach } = fixture(null);
	let audio: { playbackRate: number } | undefined;
	const stale = createLivePlaybackSpeed(() => audio && { audio, matches: () => true });
	stale.restore(storage);
	attach();
	speed.set(1.5);
	audio = { playbackRate: 1 };
	stale.attach();
	expect(storage.getItem()).toBe('1.5');
});

test('renders clamped readback and keeps mismatch feedback through the write’s ratechange', () => {
	const { speed, attach, storage } = fixture();
	const { current, apply, native } = attach();
	apply.mockImplementationOnce(() => native(1.25));
	expect(speed.set(2)).toBe(1.25);
	expect(storage.getItem()).toBe('1.25');
	const mismatch = { rate: 1.25, feedback: 'Speed 2x not applied — still 1.25x' };
	expect(get(speed.state)).toEqual(mismatch);
	speed.observe(current);
	expect(get(speed.state)).toEqual(mismatch);
	native(1.75);
	expect(get(speed.state)).toEqual({ rate: 1.75, feedback: '' });
	expect(storage.getItem()).toBe('1.75');
	expect(apply.mock.calls).toEqual([[1.3], [2]]);
});

test('a throwing restore reports the observed rate without replacing the stored preference', () => {
	const { speed, attach, storage, retire } = fixture();
	const { apply, current } = attach();
	apply.mockImplementationOnce(() => { throw new Error('refused'); });
	speed.set(2);
	expect(get(speed.state)).toEqual({ rate: 1.3, feedback: 'Speed 2x not applied — still 1.3x' });
	expect(storage.setItem).toHaveBeenCalledTimes(1);
	retire();
	speed.observe(current);
	expect(get(speed.state).rate).toBeNull();
	attach(() => { throw new Error('refused'); });
	expect(get(speed.state)).toEqual({ rate: 1, feedback: 'Speed 1.3x not applied — still 1x' });
	expect(storage.setItem).toHaveBeenCalledTimes(1);
	expect(storage.getItem()).toBe('1.3');
});

test.each([1, 1.25])('a source restoring to %sx reports the mismatch and persists verified readback', (rate) => {
	const { speed, storage, attach } = fixture();
	attach();
	const { current } = attach(() => rate);
	speed.observe(current);
	expect(get(speed.state)).toEqual({ rate, feedback: `Speed 1.3x not applied — still ${rate}x` });
	expect(storage.getItem()).toBe(String(rate));
});

test('retired source readback cannot publish or persist, and the next source restores the last verified rate', () => {
	const { speed, storage, attach, retire } = fixture();
	const old = attach();
	speed.set(1.75);
	old.apply.mockImplementationOnce(retire);
	expect(speed.set(2)).toBeNull();
	expect(speed.set(0.5)).toBeNull();
	expect(storage.setItem).toHaveBeenCalledTimes(2);
	const fresh = attach();
	old.native(0.5);
	expect(fresh.audio.playbackRate).toBe(1.75);
	expect(get(speed.state)).toEqual({ rate: 1.75, feedback: '' });
	expect(storage.getItem()).toBe('1.75');
});
