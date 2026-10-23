import { describe, expect, it, vi } from 'vitest';
import {
	applySoundPreference,
	loadSoundPreference,
	parseSoundPreference,
	saveSoundPreference,
	serializeSoundPreference,
	SOUND_PREFERENCE_STORAGE_KEY
} from './soundPreference.js';

const defaults = { volume: 1, muted: false };

describe('device sound preference wire format', () => {
	it.each([0, 0.35, 1])('round-trips volume %s with either mute state', (volume) => {
		for (const muted of [true, false]) {
			const value = { volume, muted };
			const raw = serializeSoundPreference(value);
			expect(JSON.parse(raw)).toEqual({ version: 1, ...value });
			expect(parseSoundPreference(raw)).toEqual(value);
		}
	});

	it.each([
		null, '', '{', 'null', '[]', 'true', '0', '"0.35"',
		'{"version":2,"volume":0.35,"muted":true}',
		'{"version":"1","volume":0.35,"muted":true}',
		'{"volume":0.35,"muted":true}',
		'{"version":1,"muted":true}',
		'{"version":1,"volume":0.35}',
		'{"version":1,"volume":"0.35","muted":true}',
		'{"version":1,"volume":null,"muted":true}',
		'{"version":1,"volume":true,"muted":true}',
		'{"version":1,"volume":{},"muted":true}',
		'{"version":1,"volume":-0.01,"muted":true}',
		'{"version":1,"volume":1.01,"muted":true}',
		'{"version":1,"volume":1e999,"muted":true}',
		'{"version":1,"volume":-1e999,"muted":true}',
		'{"version":1,"volume":NaN,"muted":true}',
		'{"version":1,"volume":0.35,"muted":"false"}',
		'{"version":1,"volume":0.35,"muted":0}',
		'{"version":1,"volume":0.35,"muted":null}'
	])('defaults atomically for invalid record %s', (raw) => {
		expect(parseSoundPreference(raw)).toEqual(defaults);
	});

	it.each([NaN, Infinity, -Infinity, -0.01, 1.01])('never serializes invalid volume %s', (volume) => {
		expect(JSON.parse(serializeSoundPreference({ volume, muted: true }))).toEqual({ version: 1, ...defaults });
	});

	it('strips unrelated data when loading and serializing', () => {
		const value = { volume: 0.35, muted: true, trackId: 'synthetic-track' };
		expect(parseSoundPreference(JSON.stringify({ version: 1, ...value }))).toEqual({ volume: 0.35, muted: true });
		expect(JSON.parse(serializeSoundPreference(value))).toEqual({ version: 1, volume: 0.35, muted: true });
	});

	it('returns independent defaults rather than shared mutable state', () => {
		parseSoundPreference(null).volume = 0;
		expect(parseSoundPreference(null)).toEqual(defaults);
	});
});

describe('fail-soft device storage', () => {
	it('reads only the namespaced key and does not write during load', () => {
		const storage = {
			getItem: vi.fn(() => '{"version":1,"volume":0,"muted":true}'),
			setItem: vi.fn()
		};
		expect(loadSoundPreference(storage)).toEqual({ volume: 0, muted: true });
		expect(storage.getItem).toHaveBeenCalledExactlyOnceWith(SOUND_PREFERENCE_STORAGE_KEY);
		expect(storage.setItem).not.toHaveBeenCalled();
		saveSoundPreference(storage, { volume: 0.35, muted: false });
		expect(storage.setItem).toHaveBeenCalledExactlyOnceWith('nntune:sound-preference:v1', '{"version":1,"volume":0.35,"muted":false}');
	});

	it('survives null storage, read denial and quota failure', () => {
		const storage = {
			getItem() { throw new Error('blocked'); },
			setItem() { throw new Error('full'); }
		};
		expect(loadSoundPreference(null)).toEqual(defaults);
		expect(loadSoundPreference(storage)).toEqual(defaults);
		expect(() => saveSoundPreference(null, defaults)).not.toThrow();
		expect(() => saveSoundPreference(storage, defaults)).not.toThrow();
	});
});

describe('element-only sound application', () => {
	it('preserves zero/mute and leaves transport, rate, time and gain alone', () => {
		const element = { volume: 1, muted: false, paused: true, currentTime: 5, playbackRate: 1.5, gain: 0.7, play: vi.fn() };
		expect(applySoundPreference(element, { volume: 0, muted: true })).toEqual({ volume: 0, muted: true, volumeRestricted: false });
		expect(element).toMatchObject({ volume: 0, muted: true, paused: true, currentTime: 5, playbackRate: 1.5, gain: 0.7 });
		expect(element.play).not.toHaveBeenCalled();
	});

	it('does not assign equal values (no event amplification)', () => {
		const setVolume = vi.fn();
		const setMuted = vi.fn();
		const element = { get volume() { return 0.35; }, set volume(v: number) { setVolume(v); }, get muted() { return true; }, set muted(v: boolean) { setMuted(v); } };
		applySoundPreference(element, { volume: 0.35, muted: true });
		expect(setVolume).not.toHaveBeenCalled();
		expect(setMuted).not.toHaveBeenCalled();
	});

	it.each(['throw', 'ignore'])('reports actual volume for a setter that will %s, still applies mute', (mode) => {
		const element = { get volume() { return 1; }, set volume(_v: number) { if (mode === 'throw') throw new Error('unsupported'); }, muted: false };
		expect(applySoundPreference(element, { volume: 0.35, muted: true })).toEqual({ volume: 1, muted: true, volumeRestricted: true });
	});

	it('tolerates a restricted mute setter independently of volume', () => {
		const element = { volume: 1, get muted() { return false; }, set muted(_v: boolean) { throw new Error('unsupported'); } };
		expect(applySoundPreference(element, { volume: 0.35, muted: true })).toEqual({ volume: 0.35, muted: false, volumeRestricted: false });
	});
});
