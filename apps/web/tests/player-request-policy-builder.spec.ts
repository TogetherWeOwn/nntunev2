import { Buffer } from 'node:buffer';
import { expect, test, type Page } from '@playwright/test';
import { openQueue } from './helpers/queue-host';

// Unknown extras deliberately resemble real tags. They are not an adopted
// wire schema, nor evidence of measured loudness or served-rendition quality.
const ITEMS = ['a', 'b', 'c'].map((id) => ({
	id: `policy-${id}`, title: `Policy ${id.toUpperCase()}`, artist: 'Test artist',
	streamUrl: `/test-audio/policy-${id}`,
	trackGainDb: '+18 dB', trackPeak: 2, albumGainDb: -20, albumPeak: 3,
	format: 'opus', codec: 'opus', bitrate: 64, sampleRate: 48000,
	quality: { codec: 'flac', bitrate: 1411 }, leveling: { trackGainDb: 30 }
}));

type Probe = { graphs: number; sources: string[]; calls: string[]; releasePlay?: () => void };
declare global { interface Window { __requestPolicy: Probe } }

function silentWav(): Buffer {
	const bytes = 60 * 8000 * 2;
	const wav = Buffer.alloc(44 + bytes);
	wav.write('RIFF'); wav.writeUInt32LE(36 + bytes, 4); wav.write('WAVEfmt ', 8);
	wav.writeUInt32LE(16, 16); wav.writeUInt16LE(1, 20); wav.writeUInt16LE(1, 22);
	wav.writeUInt32LE(8000, 24); wav.writeUInt32LE(16000, 28);
	wav.writeUInt16LE(2, 32); wav.writeUInt16LE(16, 34);
	wav.write('data', 36); wav.writeUInt32LE(bytes, 40);
	return wav;
}

async function setup(page: Page, enabled = true) {
	const traffic: { method: string; origin: string; path: string; type: string }[] = [];
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(error.message));
	page.on('request', (request) => traffic.push({
		method: request.method(), origin: new URL(request.url()).origin,
		path: new URL(request.url()).pathname, type: request.resourceType()
	}));
	await page.addInitScript((enabled) => {
		localStorage.setItem('nntune.leveling.enabled', enabled ? '1' : '0');
		window.__requestPolicy = { graphs: 0, sources: [], calls: [] };
		// An unavailable graph must not gate native playback. A constructor call
		// is itself a failure, even if production catches the exception.
		for (const name of ['AudioContext', 'webkitAudioContext']) {
			Object.defineProperty(window, name, { configurable: true, value: class {
				constructor() { window.__requestPolicy.graphs++; throw new Error('WebAudio unavailable'); }
			} });
		}
		const record = (node: Node) => {
			if (!(node instanceof Element)) return;
			for (const media of [node, ...node.querySelectorAll('audio, video, source')]) {
				if (media.matches('audio, video, source')) {
					const src = media.getAttribute('src');
					if (src) window.__requestPolicy.sources.push(src);
				}
			}
		};
		new MutationObserver((records) => {
			for (const recordEntry of records) {
				if (recordEntry.type === 'attributes') record(recordEntry.target);
				for (const node of recordEntry.addedNodes) record(node);
			}
		}).observe(document, { subtree: true, childList: true, attributes: true, attributeFilter: ['src'] });
		for (const name of ['play', 'pause', 'load'] as const) {
			const original = HTMLMediaElement.prototype[name];
			Object.defineProperty(HTMLMediaElement.prototype, name, { configurable: true, value: function (this: HTMLMediaElement) {
				window.__requestPolicy.calls.push(`${name}:${this.getAttribute('src')}`);
				return original.call(this);
			} });
		}
	}, enabled);
	await page.route('**/api/library', (route) => route.fulfill({ json: ITEMS }));
	const wav = silentWav();
	await page.route('**/test-audio/**', (route) => {
		const range = /^bytes=(\d+)-(\d*)$/.exec(route.request().headers().range ?? '');
		const start = range ? Number(range[1]) : 0;
		const end = range?.[2] ? Number(range[2]) : wav.length - 1;
		return route.fulfill({
			status: range ? 206 : 200, contentType: 'audio/wav', body: wav.subarray(start, end + 1),
			headers: { 'Accept-Ranges': 'bytes', 'Content-Length': String(end - start + 1),
				...(range ? { 'Content-Range': `bytes ${start}-${end}/${wav.length}` } : {}) }
		});
	});
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(3);
	for (const item of ITEMS) {
		await page.locator(`[data-testid="library-item"][data-id="${item.id}"]`)
			.getByRole('button', { name: new RegExp(`^${item.title} —`) }).click();
	}
	await expect(page.getByTestId('audio-player')).toHaveJSProperty('readyState', 4);
	return { traffic, errors };
}

async function settled(page: Page) {
	await page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
}

async function assertOnlyCurrent(page: Page, traffic: Awaited<ReturnType<typeof setup>>['traffic'], allowed: string[]) {
	await settled(page);
	// Observe actual browser resource requests, not just window.fetch. HEAD,
	// range GET, grants and media-element downloads all reach this ledger.
	const dynamic = traffic.filter(({ type, path }) => ['media', 'fetch', 'xhr'].includes(type) ||
		path.startsWith('/api/') || path.startsWith('/v1/') || path.startsWith('/test-audio/'));
	for (const request of dynamic) {
		expect(request.method).toBe('GET');
		expect(request.origin).toBe(new URL(page.url()).origin);
		expect(['/api/library', ...allowed]).toContain(request.path);
	}
	const probe = await page.evaluate(() => window.__requestPolicy);
	expect(probe.graphs).toBe(0);
	for (const source of probe.sources) expect(allowed).toContain(source);
	for (const call of probe.calls) expect(allowed).toContain(call.slice(call.indexOf(':') + 1));
	await expect(page.locator('audio, video')).toHaveCount(1);
}

for (const enabled of [true, false]) {
	test(`unverified metadata leaves native sound unchanged and never warms upcoming sources with leveling ${enabled ? 'enabled' : 'bypassed'}`, async ({ page }) => {
		const { traffic, errors } = await setup(page, enabled);
		const audio = page.getByTestId('audio-player');
		const sourceA = ITEMS[0].streamUrl;
		await page.getByTestId('player-volume').fill('35');
		await page.getByRole('button', { name: 'Mute', exact: true }).click();
		await expect(audio).toHaveJSProperty('volume', 0.35);
		await expect(audio).toHaveJSProperty('muted', true);
		await openQueue(page);
		await page.locator('[data-testid="queue-item"][data-track-id="policy-b"]').hover();
		await assertOnlyCurrent(page, traffic, [sourceA]);

		await page.getByTestId('player-play-pause').click();
		await expect(audio).toHaveJSProperty('paused', false);
		await page.getByTestId('player-seek').fill('58');
		await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBeGreaterThan(58.2);
		await page.getByTestId('player-play-pause').click();
		await expect(audio).toHaveJSProperty('paused', true);
		await assertOnlyCurrent(page, traffic, [sourceA]);
		await page.locator('nav[aria-label="Primary"] a[href="/home"]:visible').click();
		await expect(page).toHaveURL(/\/home(?:\?|$)/);
		await expect(audio).toHaveJSProperty('volume', 0.35);
		await expect(audio).toHaveJSProperty('muted', true);
		await assertOnlyCurrent(page, traffic, [sourceA]);

		// Positive control: B's valid URL is already queued, but first becomes
		// requestable when explicit Next makes it current, never on hover/near-end.
		await page.getByTestId('player-next').click();
		await expect(audio).toHaveAttribute('src', ITEMS[1].streamUrl);
		await expect(audio).toHaveJSProperty('readyState', 4);
		expect(traffic.some(({ path, type }) => path === ITEMS[1].streamUrl && type === 'media')).toBe(true);
		await expect(audio).toHaveJSProperty('volume', 0.35);
		await expect(audio).toHaveJSProperty('muted', true);
		await assertOnlyCurrent(page, traffic, [sourceA, ITEMS[1].streamUrl]);
		expect(errors).toEqual([]);
	});
}

for (const boundary of ['clear', 'auth', 'replacement'] as const) {
	test(`a stale native play continuation cannot warm upcoming sources after ${boundary}`, async ({ page }) => {
		const { traffic, errors } = await setup(page);
		await page.evaluate(() => {
			const original = HTMLMediaElement.prototype.play;
			let first = true;
			HTMLMediaElement.prototype.play = function () {
				const native = original.call(this);
				if (!first) return native;
				first = false;
				return new Promise<void>((resolve, reject) => {
					native.then(() => { window.__requestPolicy.releasePlay = resolve; }, reject);
				});
			};
		});
		await page.getByTestId('player-play-pause').click();
		await expect(page.getByTestId('audio-player')).toHaveJSProperty('paused', false);
		await expect.poll(() => page.evaluate(() => typeof window.__requestPolicy.releasePlay)).toBe('function');
		if (boundary === 'clear') {
			await openQueue(page);
			await page.getByRole('button', { name: 'Clear queue', exact: true }).click();
			await page.getByRole('button', { name: /^(confirm|yes\b|clear\s+\d+)/i }).click();
		} else if (boundary === 'auth') {
			const otherTab = await page.context().newPage();
			await otherTab.goto('/login');
			await otherTab.evaluate(() => {
				localStorage.setItem('nntune.library.auth-transition.v1', 'request-policy-change');
				localStorage.removeItem('nntune.library.auth-transition.v1');
			});
			await expect(page.getByTestId('audio-player')).toHaveCount(0);
			await otherTab.close();
		} else {
			await page.getByTestId('player-next').click();
			await expect(page.getByTestId('audio-player')).toHaveAttribute('src', ITEMS[1].streamUrl);
			await expect(page.getByTestId('audio-player')).toHaveJSProperty('readyState', 4);
		}
		await page.evaluate(() => window.__requestPolicy.releasePlay!());
		await settled(page);
		if (boundary === 'replacement') {
			await assertOnlyCurrent(page, traffic, ITEMS.slice(0, 2).map((item) => item.streamUrl));
		} else {
			await expect(page.locator('audio, video')).toHaveCount(0);
			const media = traffic.filter(({ type, path }) => type === 'media' || path.startsWith('/test-audio/'));
			expect(media.length).toBeGreaterThan(0);
			expect(media.every(({ path, method }) => path === ITEMS[0].streamUrl && method === 'GET')).toBe(true);
			expect(traffic.filter(({ type, path }) => ['fetch', 'xhr'].includes(type) || path.startsWith('/v1/')))
				.toEqual([{ method: 'GET', origin: new URL(page.url()).origin, path: '/api/library', type: 'fetch' }]);
		}
		expect(errors).toEqual([]);
	});
}
