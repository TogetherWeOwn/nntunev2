import { expect, type Page } from '@playwright/test';
import { SOUND_KEY, OTHER_KEY, frames } from './sound-preference/fixtures';

export type SoundProbeOptions = {
	raw?: string | null;
	storage?: 'normal' | 'getter-blocked' | 'read-blocked' | 'write-blocked';
	volume?: 'normal' | 'rejected' | 'fixed' | 'clamped';
};

type SoundProbe = {
	plays: { volume: number; muted: boolean }[];
	pause: number;
	load: number;
	volumeWrites: number;
	muteWrites: number;
	events: number;
	writeAttempts: number;
	soundWrites: string[];
	volumeMode: NonNullable<SoundProbeOptions['volume']>;
	read: (key: string) => string | null;
};

declare global {
	interface Window {
		__librarySoundProbe: SoundProbe;
	}
}

export const ITEMS = [
	{ id: 'libsound-coral', title: 'Coral Night', artist: 'Mira', streamUrl: '/test-audio/libsound-coral' },
	{ id: 'libsound-pine', title: 'Pine Morning', artist: 'North', streamUrl: '/test-audio/libsound-pine' },
];

function silentWav() {
	const size = 16 * 8_000 * 2;
	const wav = Buffer.alloc(44 + size);
	wav.write('RIFF', 0);
	wav.writeUInt32LE(36 + size, 4);
	wav.write('WAVEfmt ', 8);
	wav.writeUInt32LE(16, 16);
	wav.writeUInt16LE(1, 20);
	wav.writeUInt16LE(1, 22);
	wav.writeUInt32LE(8_000, 24);
	wav.writeUInt32LE(16_000, 28);
	wav.writeUInt16LE(2, 32);
	wav.writeUInt16LE(16, 34);
	wav.write('data', 36);
	wav.writeUInt32LE(size, 40);
	return wav;
}

// Keep native media accessors/events and real PCM playback. Only simulate the
// device refusal boundary; never implement the preference or its UI in a fake.
export async function openLibrarySound(page: Page, options: SoundProbeOptions = {}) {
	const errors: string[] = [];
	const unexpected: string[] = [];
	const api: string[] = [];
	const wav = silentWav();
	let signedOut = false;
	page.on('pageerror', (error) => errors.push(error.message));
	await page.route('**/*', (route) => {
		const request = route.request();
		const url = new URL(request.url());
		if (!['localhost', '127.0.0.1', '[::1]'].includes(url.hostname)) {
			unexpected.push(`${request.method()} ${url.origin}${url.pathname}`);
			return route.abort();
		}
		if (url.pathname === '/api/library' && request.method() === 'GET') {
			api.push('GET /api/library');
			return route.fulfill({ status: signedOut ? 401 : 200,
				contentType: 'application/json', body: signedOut ? '{}' : JSON.stringify(ITEMS) });
		}
		if (ITEMS.some((item) => url.pathname === item.streamUrl) && request.method() === 'GET') {
			const rangeHeader = request.headers()['range'];
			const range = /^bytes=(\d+)-(\d*)$/.exec(rangeHeader ?? '');
			const start = range ? Number(range[1]) : 0;
			const end = range?.[2] ? Math.min(Number(range[2]), wav.length - 1) : wav.length - 1;
			if ((rangeHeader && !range) || start >= wav.length || start > end) {
				return route.fulfill({ status: 416, headers: { 'content-range': `bytes */${wav.length}` }, body: '' });
			}
			return route.fulfill({ status: range ? 206 : 200, contentType: 'audio/wav',
				headers: { 'accept-ranges': 'bytes', 'content-length': String(end - start + 1),
					...(range ? { 'content-range': `bytes ${start}-${end}/${wav.length}` } : {}) },
				body: wav.subarray(start, end + 1) });
		}
		if (request.method() !== 'GET' || /^\/(?:api|v1|test-audio)(?:\/|$)/.test(url.pathname)) {
			unexpected.push(`${request.method()} ${url.pathname}`);
			return route.abort();
		}
		return route.continue();
	});
	await page.addInitScript(({ key, other, options }) => {
		const { raw, storage = 'normal', volume = 'normal' } = options;
		const backing = window.localStorage;
		const get = Storage.prototype.getItem;
		const set = Storage.prototype.setItem;
		const seedKey = 'library-sound-test-seeded';
		if (!get.call(sessionStorage, seedKey)) {
			if (raw === null) backing.removeItem(key);
			else if (raw !== undefined) set.call(backing, key, raw);
			set.call(backing, other, 'keep-me');
			set.call(sessionStorage, seedKey, 'yes');
		}
		const probe: SoundProbe = {
			plays: [], pause: 0, load: 0, volumeWrites: 0, muteWrites: 0,
			events: 0, writeAttempts: 0, soundWrites: [], volumeMode: volume,
			read: (name) => get.call(backing, name),
		};
		window.__librarySoundProbe = probe;
		Storage.prototype.getItem = function (name: string) {
			if (this === backing && name === key && storage === 'read-blocked') {
				throw new DOMException('Device storage unavailable', 'SecurityError');
			}
			return get.call(this, name);
		};
		Storage.prototype.setItem = function (name: string, value: string) {
			if (this === backing && name === key) {
				probe.writeAttempts += 1;
				if (storage === 'write-blocked' || storage === 'read-blocked') {
					throw new DOMException('Device storage unavailable', 'QuotaExceededError');
				}
				probe.soundWrites.push(String(value));
			}
			return set.call(this, name, value);
		};
		if (storage === 'getter-blocked') Object.defineProperty(window, 'localStorage', {
			configurable: true,
			get() { throw new DOMException('Device storage unavailable', 'SecurityError'); },
		});
		for (const name of ['volume', 'muted'] as const) {
			const descriptor = Object.getOwnPropertyDescriptor(HTMLMediaElement.prototype, name)!;
			Object.defineProperty(HTMLMediaElement.prototype, name, {
				...descriptor,
				set(value: number | boolean) {
					if (name === 'volume') {
						probe.volumeWrites += 1;
						if (probe.volumeMode === 'rejected') {
							throw new DOMException('Use system volume controls', 'NotSupportedError');
						}
						if (probe.volumeMode === 'fixed') return;
						if (probe.volumeMode === 'clamped') value = Math.min(Number(value), 0.25);
					} else probe.muteWrites += 1;
					descriptor.set!.call(this, value);
				},
			});
		}
		const play = HTMLMediaElement.prototype.play;
		const pause = HTMLMediaElement.prototype.pause;
		const load = HTMLMediaElement.prototype.load;
		HTMLMediaElement.prototype.play = function () {
			probe.plays.push({ volume: this.volume, muted: this.muted });
			return play.call(this);
		};
		HTMLMediaElement.prototype.pause = function () { probe.pause += 1; return pause.call(this); };
		HTMLMediaElement.prototype.load = function () { probe.load += 1; return load.call(this); };
		document.addEventListener('volumechange', () => { probe.events += 1; }, true);
	}, { key: SOUND_KEY, other: OTHER_KEY, options });
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(2);
	await selectTrack(page, 0);
	return { errors, unexpected, api, setSignedOut: (value: boolean) => { signedOut = value; } };
}

export async function selectTrack(page: Page, index: number) {
	await page.locator(`[data-testid="library-item"][data-id="${ITEMS[index].id}"]`)
		.getByRole('button', { name: new RegExp(`^${ITEMS[index].title}( — |$)`) }).click();
	await expect(page.getByTestId('audio-player')).toBeAttached();
	await expect.poll(() => page.getByTestId('audio-player').evaluate((audio: HTMLAudioElement) => audio.readyState))
		.toBeGreaterThanOrEqual(1);
	await frames(page);
}

export async function mediaState(page: Page) {
	return page.getByTestId('audio-player').evaluate((audio: HTMLAudioElement) => ({
		volume: audio.volume, muted: audio.muted, src: audio.getAttribute('src'),
		time: audio.currentTime, controls: audio.controls, paused: audio.paused, rate: audio.playbackRate,
	}));
}

export async function soundEffects(page: Page) {
	return page.evaluate(() => {
		const { plays, pause, load, volumeWrites, muteWrites, writeAttempts, soundWrites, events } = window.__librarySoundProbe;
		return { plays: [...plays], pause, load, volumeWrites, muteWrites, writeAttempts, soundWrites: [...soundWrites], events };
	});
}

export async function nativeSound(page: Page, volume: number, muted: boolean) {
	await page.getByTestId('audio-player').evaluate((audio: HTMLAudioElement, values) => {
		audio.volume = values.volume;
		audio.muted = values.muted;
		audio.dispatchEvent(new Event('volumechange'));
	}, { volume, muted });
	await frames(page);
}

export async function preference(page: Page) {
	return page.evaluate((key) => {
		const raw = window.__librarySoundProbe.read(key);
		return raw === null ? null : JSON.parse(raw);
	}, SOUND_KEY);
}
