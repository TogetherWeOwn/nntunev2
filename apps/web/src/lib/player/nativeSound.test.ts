import { get } from 'svelte/store';
import { describe, expect, it, vi } from 'vitest';
import { createNativeSound } from './nativeSound.js';
import { SOUND_PREFERENCE_STORAGE_KEY } from './soundPreference.js';

class Audio extends EventTarget {
	level = 1;
	muted = false;
	mode: 'accept' | 'ignore' | 'throw' | 'clamp' = 'accept';
	get volume() { return this.level; }
	set volume(value: number) {
		if (this.mode === 'throw') throw new Error('System volume only');
		if (this.mode === 'accept') this.level = value;
		if (this.mode === 'clamp') this.level = Math.max(0.5, value);
	}
	changed() { this.dispatchEvent(new Event('volumechange')); }
}

function stored(volume: number, muted: boolean) {
	return JSON.stringify({ version: 1, volume, muted });
}

function setup(raw: string | null = null) {
	const storage = { getItem: vi.fn(() => raw), setItem: vi.fn() };
	const sound = createNativeSound(() => storage);
	let current: Audio | undefined;
	function connect(audio: Audio) {
		current = audio;
		const abort = new AbortController();
		sound.connect(audio, () => current === audio, abort.signal);
		return abort;
	}
	return { storage, sound, connect, read: () => get(sound.state) };
}

describe('persistent native sound', () => {
	it('restores zero and mute, never writes on setup, and persists only changed native readback', () => {
		const { storage, sound, connect, read } = setup(stored(0, true));
		const audio = new Audio();
		connect(audio);
		expect(read()).toEqual({ volume: 0, muted: true, ready: true, restricted: false });
		audio.changed();
		expect(storage.setItem).not.toHaveBeenCalled();
		expect(sound.setVolume(0.35)).toBe(0.35);
		expect(audio.muted).toBe(true);
		audio.changed();
		expect(storage.setItem).toHaveBeenCalledExactlyOnceWith(SOUND_PREFERENCE_STORAGE_KEY, stored(0.35, true));
		audio.muted = false;
		audio.volume = 0.42;
		audio.changed();
		expect(read()).toMatchObject({ volume: 0.42, muted: false });
		expect(storage.setItem).toHaveBeenLastCalledWith(SOUND_PREFERENCE_STORAGE_KEY, stored(0.42, false));
	});

	it('merges into valid storage without adopting another tab’s mute on replacement', () => {
		const { storage, sound, connect } = setup(stored(1, false));
		const old = connect(new Audio());
		storage.getItem.mockReturnValue(stored(1, true));
		sound.setVolume(0.3);
		expect(storage.setItem).toHaveBeenLastCalledWith(SOUND_PREFERENCE_STORAGE_KEY, stored(0.3, true));
		old.abort();
		const replacement = new Audio();
		connect(replacement);
		expect(replacement.muted).toBe(false);
		storage.getItem.mockReturnValue('{"version":1,"volume":99,"muted":false}');
		sound.toggleMute();
		expect(storage.setItem).toHaveBeenCalledTimes(2);
		expect(storage.setItem).toHaveBeenLastCalledWith(SOUND_PREFERENCE_STORAGE_KEY, stored(0.3, true));
	});

	it.each(['ignore', 'throw'] as const)('keeps mute usable without overwriting a saved volume when setters %s', (mode) => {
		const { storage, sound, connect, read } = setup(stored(0.35, true));
		const audio = new Audio();
		audio.mode = mode;
		const old = connect(audio);
		expect(read()).toEqual({ volume: 1, muted: true, restricted: true, ready: true });
		audio.changed();
		expect(storage.setItem).not.toHaveBeenCalled();
		sound.toggleMute();
		expect(audio.muted).toBe(false);
		expect(storage.setItem).toHaveBeenCalledExactlyOnceWith(SOUND_PREFERENCE_STORAGE_KEY, stored(0.35, false));
		old.abort();
		const replacement = new Audio();
		connect(replacement);
		expect(replacement.volume).toBe(0.35);
		expect(read()).toMatchObject({ volume: 0.35, muted: false, restricted: true });
	});

	it('reports a clamped volume without replacing the saved choice', () => {
		const { storage, sound, connect, read } = setup(stored(0.8, false));
		const audio = new Audio();
		audio.mode = 'clamp';
		const old = connect(audio);
		expect(sound.setVolume(0.35)).toBe(0.5);
		expect(read()).toMatchObject({ volume: 0.5, muted: false, restricted: true });
		audio.changed();
		expect(storage.setItem).not.toHaveBeenCalled();
		sound.toggleMute();
		expect(read()).toMatchObject({ volume: 0.5, muted: true, restricted: true });
		expect(storage.setItem).toHaveBeenCalledExactlyOnceWith(SOUND_PREFERENCE_STORAGE_KEY, stored(0.8, true));
		old.abort();
		const replacement = new Audio();
		connect(replacement);
		expect(replacement.volume).toBe(0.8);
		expect(replacement.muted).toBe(true);
	});

	it('fences stale events and callbacks around retirement and replacement, even with denied storage', () => {
		const { storage, sound, connect, read } = setup();
		storage.getItem.mockImplementation(() => { throw new Error('Storage denied'); });
		storage.setItem.mockImplementation(() => { throw new Error('Storage denied'); });
		const audio = new Audio();
		const retired = connect(audio);
		sound.setVolume(0.61);
		sound.toggleMute();
		retired.abort();
		expect(read().ready).toBe(false);
		sound.setVolume(0);
		sound.toggleMute();
		expect(audio.volume).toBe(0.61);
		expect(audio.muted).toBe(true);
		const next = new Audio();
		connect(next);
		audio.level = 0;
		audio.muted = false;
		audio.changed();
		expect(read()).toEqual({ volume: 0.61, muted: true, ready: true, restricted: false });
		expect(storage.setItem).not.toHaveBeenCalled();
	});

	it('keeps controls usable if acquiring storage throws', () => {
		const sound = createNativeSound(() => { throw new Error('Storage denied'); });
		sound.connect(new Audio(), () => true, new AbortController().signal);
		expect(sound.setVolume(0.4)).toBe(0.4);
		sound.toggleMute();
		expect(get(sound.state)).toMatchObject({ ready: true, volume: 0.4, muted: true });
	});
});
