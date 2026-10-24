import { expect, type Page } from '@playwright/test';

// Public wire contract, not an implementation import. Keep fixtures literal.
export const SOUND_KEY = 'nntune:sound-preference:v1';
export const OTHER_KEY = 'acceptance:unrelated-setting';
export const saved35 = '{"version":1,"volume":0.35,"muted":true}';

export type Options = {
	saved?: string;
	storage?: 'getter' | 'read' | 'write';
	volumeSetter?: 'throw' | 'ignore';
	url?: string;
};

export type SoundProbe = {
	plays: { volume: number; muted: boolean }[];
	writes: { key: string; value: string }[];
	events: number;
	setterAttempts: number;
	read: (key: string) => string | null;
};

declare global {
	interface Window {
		__soundAcceptance: SoundProbe;
	}
}

// Real browser media with a local, generated 16-second silent PCM WAV. No
// backend/provider or copyrighted track, and no fake volume/muted properties.
function silentWav(): Buffer {
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

export async function openPlayer(page: Page, options: Options = {}) {
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(error.message));
	const wav = silentWav();
	await page.route('**/api/demo/audio**', (route) => {
		const range = /^bytes=(\d+)-(\d*)$/.exec(route.request().headers()['range'] ?? '');
		const start = range ? Number(range[1]) : 0;
		const end = range?.[2] ? Math.min(Number(range[2]), wav.length - 1) : wav.length - 1;
		// Chromium needs a truthful range response to seek a paused WAV. A 200
		// answer to its Range request can make currentTime snap back to zero.
		if (start >= wav.length || start > end) return route.fulfill({
			status: 416, headers: { 'content-range': `bytes */${wav.length}` }, body: ''
		});
		return route.fulfill({
			status: range ? 206 : 200,
			contentType: 'audio/wav',
			headers: {
				'accept-ranges': 'bytes',
				'content-length': String(end - start + 1),
				...(range ? { 'content-range': `bytes ${start}-${end}/${wav.length}` } : {})
			},
			body: wav.subarray(start, end + 1)
		});
	});
	await page.addInitScript(({ options, key, other }) => {
		const backing = window.localStorage;
		const get = Storage.prototype.getItem;
		const set = Storage.prototype.setItem;
		// Seed only once, so reopening/reloading tests the app's own write.
		if (!sessionStorage.getItem('acceptance:sound-seeded')) {
			sessionStorage.setItem('acceptance:sound-seeded', '1');
			set.call(backing, other, 'keep-me');
			if (options.saved !== undefined) set.call(backing, key, options.saved);
		}
		const probe: SoundProbe = {
			plays: [], writes: [], events: 0, setterAttempts: 0,
			read: (name) => get.call(backing, name)
		};
		window.__soundAcceptance = probe;
		Storage.prototype.getItem = function (name) {
			if (this === backing && name === key && options.storage === 'read') {
				throw new DOMException('Storage blocked', 'SecurityError');
			}
			return get.call(this, name);
		};
		Storage.prototype.setItem = function (name, value) {
			if (this === backing) {
				probe.writes.push({ key: name, value: String(value) });
				if (name === key && options.storage === 'write') {
					throw new DOMException('Storage full', 'QuotaExceededError');
				}
			}
			return set.call(this, name, value);
		};
		if (options.storage === 'getter') {
			Object.defineProperty(window, 'localStorage', {
				configurable: true,
				get() { throw new DOMException('Storage blocked', 'SecurityError'); }
			});
		}
		const play = HTMLMediaElement.prototype.play;
		HTMLMediaElement.prototype.play = function () {
			probe.plays.push({ volume: this.volume, muted: this.muted });
			return play.call(this);
		};
		document.addEventListener('volumechange', () => { probe.events += 1; }, true);
		// Fault injection only in SP-SETTER cases. All other tests keep native
		// media accessors and native volumechange event scheduling untouched.
		if (options.volumeSetter) {
			const descriptor = Object.getOwnPropertyDescriptor(HTMLMediaElement.prototype, 'volume')!;
			Object.defineProperty(HTMLMediaElement.prototype, 'volume', {
				...descriptor,
				set() {
					probe.setterAttempts += 1;
					if (options.volumeSetter === 'throw') {
						throw new DOMException('Volume is platform controlled', 'NotSupportedError');
					}
				}
			});
		}
	}, { options, key: SOUND_KEY, other: OTHER_KEY });
	await page.goto(options.url ?? '/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	// Existing mount-only hook is a hydration barrier, not a new source stub.
	await page.waitForFunction(() =>
		typeof (window as unknown as { __levelingProbe?: unknown }).__levelingProbe === 'function'
	);
	await expect.poll(() => page.getByTestId('demo-audio').evaluate(
		(el: HTMLAudioElement) => el.readyState
	)).toBeGreaterThanOrEqual(1);
	await frames(page);
	return errors;
}

export function volumeControl(page: Page) {
	return page.getByRole('slider', { name: /^volume(?:\b|$)/i });
}

export function muteControl(page: Page) {
	return page.getByRole('button', { name: /^(?:mute|unmute)(?:\b|$)/i });
}

export async function media(page: Page) {
	return page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => ({
		volume: el.volume, muted: el.muted, paused: el.paused,
		rate: el.playbackRate, time: el.currentTime, controls: el.controls
	}));
}

export async function preference(page: Page) {
	return page.evaluate((key) => {
		const raw = window.__soundAcceptance.read(key);
		return raw === null ? null : JSON.parse(raw);
	}, SOUND_KEY);
}

export async function playCount(page: Page) {
	return page.evaluate(() => window.__soundAcceptance.plays.length);
}

export async function frames(page: Page) {
	await page.evaluate(async () => {
		for (let i = 0; i < 4; i += 1) await new Promise(requestAnimationFrame);
	});
}

export async function expectSound(page: Page, volume: number, muted: boolean) {
	await expect.poll(async () => {
		const state = await media(page);
		return { volume: state.volume, muted: state.muted };
	}).toEqual({ volume, muted });
}

export async function pressMute(page: Page) {
	await page.evaluate(() => {
		if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
	});
	await page.keyboard.press('m');
	await frames(page);
}
