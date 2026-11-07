import { Buffer } from 'node:buffer';
import AxeBuilder from '@axe-core/playwright';
import { expect, test, type Locator, type Page, type Route } from '@playwright/test';
import { togglePlayViaPlayerUi } from '../../../e2e/helpers/player-ui.js';
import { openQueue } from './helpers/queue-host';

/*
 * Narrow exception: the Engineering Lead authorized a one-time migration, in
 * the same pull request, of this file's physical queue host, opening, locator
 * and lifecycle setup to the persistent shell's Queue panel. Every assertion
 * and behavior vector remains protected, and an independent equivalence
 * review is required.
 */

const ITEMS = [
	{ id: 'continuity-a', title: 'Continuity A', artist: 'Test artist', streamUrl: '/test-audio/continuity-a' },
	{ id: 'continuity-b', title: 'Continuity B', artist: 'Test artist', streamUrl: '/test-audio/continuity-b' }
];

// Generated silence, not a third-party recording. Use the real browser decoder
// and native play/pause events so a detached or restarted element cannot pass.
function silentWav(): Buffer {
	const bytes = 60 * 8000 * 2;
	const wav = Buffer.alloc(44 + bytes);
	wav.write('RIFF', 0);
	wav.writeUInt32LE(36 + bytes, 4);
	wav.write('WAVEfmt ', 8);
	wav.writeUInt32LE(16, 16);
	wav.writeUInt16LE(1, 20);
	wav.writeUInt16LE(1, 22);
	wav.writeUInt32LE(8000, 24);
	wav.writeUInt32LE(16000, 28);
	wav.writeUInt16LE(2, 32);
	wav.writeUInt16LE(16, 34);
	wav.write('data', 36);
	wav.writeUInt32LE(bytes, 40);
	return wav;
}

async function openLibrary(page: Page, items = ITEMS) {
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(error.message));
	const pending: Route[] = [];
	let holdRefresh = false;
	let failure: number | 'network' | null = null;
	const fulfill = (route: Route) => route.fulfill({ json: items });
	await page.route('**/api/library', (route) => {
		if (holdRefresh) pending.push(route);
		else if (failure === 'network') return route.abort('failed');
		else if (failure !== null) return route.fulfill({ status: failure, json: {} });
		else return fulfill(route);
	});
	const wav = silentWav();
	await page.route('**/test-audio/**', (route) => {
		const range = /^bytes=(\d+)-(\d*)$/.exec(route.request().headers()['range'] ?? '');
		const start = range ? Number(range[1]) : 0;
		const end = range?.[2] ? Math.min(Number(range[2]), wav.length - 1) : wav.length - 1;
		if (start > end) return route.fulfill({ status: 416, headers: { 'Content-Range': `bytes */${wav.length}` } });
		return route.fulfill({
			status: range ? 206 : 200,
			contentType: 'audio/wav',
			headers: { 'Accept-Ranges': 'bytes', ...(range ? { 'Content-Range': `bytes ${start}-${end}/${wav.length}` } : {}) },
			body: wav.subarray(start, end + 1)
		});
	});
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(items.length);
	for (const item of items) {
		await page.locator(`[data-testid="library-item"][data-id="${item.id}"]`)
			.getByRole('button', { name: new RegExp(`^${item.title}( — |$)`) }).click();
	}
	const audio = page.getByTestId('audio-player');
	await expect(audio).toHaveJSProperty('readyState', 4);
	await togglePlayViaPlayerUi(page, audio);
	await expect(audio).toHaveJSProperty('paused', false);
	await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBeGreaterThan(0.2);
	return {
		audio, errors,
		hold: () => { holdRefresh = true; },
		fail: (next: number | 'network' | null) => { failure = next; },
		release: async () => { holdRefresh = false; await Promise.all(pending.splice(0).map(fulfill)); }
	};
}

async function navigate(page: Page, path: 'home' | 'library') {
	await page.locator(`nav[aria-label="Primary"] a[href="/${path}"]:visible`).click();
	await expect(page).toHaveURL(new RegExp(`/${path}$`));
}

for (const playing of [false, true]) {
	test(`relative jumps survive Library teardown with the real ${playing ? 'playing' : 'paused'} decoder`, async ({ page }) => {
		const { audio, errors } = await openLibrary(page);
		if (!playing) {
			await togglePlayViaPlayerUi(page, audio);
			await expect(audio).toHaveJSProperty('paused', true);
		}
		await expect.poll(() => audio.evaluate((el: HTMLAudioElement) =>
			el.seekable.length ? el.seekable.end(0) : 0)).toBeGreaterThan(35);
		await audio.evaluate((el: HTMLAudioElement) => { el.currentTime = 20; });
		await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.seeking)).toBe(false);
		const original = (await audio.elementHandle())!;
		await audio.evaluate((el: HTMLAudioElement) => {
			const events: string[] = [];
			for (const name of ['play', 'pause', 'loadstart', 'emptied']) el.addEventListener(name, () => events.push(name));
			(el as HTMLAudioElement & { jumpLifecycle: string[] }).jumpLifecycle = events;
		});
		await navigate(page, 'home');
		await expect(page.getByTestId('library-item')).toHaveCount(0);
		const forward = page.getByRole('button', { name: 'Forward 10 seconds', exact: true });
		await forward.focus();
		const before = await audio.evaluate((el: HTMLAudioElement) => el.currentTime);
		await forward.press('Enter');
		await expect(forward).toBeFocused();
		await expect(page.getByTestId('player-seek-feedback')).toHaveText(/^Forward 10 seconds, now at /);
		const after = await audio.evaluate((el: HTMLAudioElement) => el.currentTime);
		expect(after - before).toBeGreaterThan(9);
		expect(after - before).toBeLessThan(12);
		const outcome = await page.getByTestId('player-seek-feedback').textContent();
		await navigate(page, 'library');
		await expect(page.getByTestId('player-seek-feedback')).toHaveText(outcome!);
		const back = page.getByRole('button', { name: 'Back 10 seconds', exact: true });
		await back.focus();
		const beforeBack = await audio.evaluate((el: HTMLAudioElement) => el.currentTime);
		await back.press('Enter');
		await expect(back).toBeFocused();
		await expect(page.getByTestId('player-seek-feedback')).toHaveText(/^Back 10 seconds, now at /);
		expect(beforeBack - await audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBeGreaterThan(8);
		expect(await original.evaluate((el) => el === document.querySelector('[data-testid="audio-player"]'))).toBe(true);
		await expect(audio).toHaveCount(1);
		await expect(audio).toHaveAttribute('src', ITEMS[0].streamUrl);
		await expect(audio).toHaveJSProperty('paused', !playing);
		expect(await audio.evaluate((el) => (el as HTMLAudioElement & { jumpLifecycle: string[] }).jumpLifecycle)).toEqual([]);
		expect(errors).toEqual([]);
	});
}

for (const outcome of ['accepted', 'blocked', 'no-stream', 'superseded'] as const) {
	test(`metadata-only queued Play survives actual Library teardown: ${outcome}`, async ({ page }) => {
		const errors: string[] = [];
		const requests: string[] = [];
		page.on('pageerror', (error) => errors.push(error.message));
		await page.setViewportSize({ width: 1280, height: 900 });
		await page.addInitScript((outcome) => {
			const scope = window as unknown as { queuedPlayCalls: number; finishQueuedPlay?: () => void; teardownMarker: string };
			scope.queuedPlayCalls = 0;
			const nativePlay = HTMLMediaElement.prototype.play;
			HTMLMediaElement.prototype.play = function () {
				scope.queuedPlayCalls += 1;
				if (outcome === 'blocked') return Promise.reject(new DOMException('Blocked', 'NotAllowedError'));
				const played = nativePlay.call(this);
				return outcome === 'superseded' ? played.then(() => new Promise<void>((resolve) => {
					scope.finishQueuedPlay = resolve;
				})) : played;
			};
		}, outcome);
		const items = [
			{ id: 'dawn', title: 'Dawn Chorus', ...(outcome === 'no-stream' ? {} : { streamUrl: '/test-audio/teardown-dawn' }) },
			{ id: 'coral', title: 'Coral Night' }
		];
		await page.route('**/api/library', (route) => route.fulfill({ json: items }));
		const wav = silentWav();
		await page.route('**/test-audio/**', (route) => {
			requests.push(route.request().url());
			const range = /^bytes=(\d+)-(\d*)$/.exec(route.request().headers().range ?? '');
			const start = range ? Number(range[1]) : 0;
			const end = range?.[2] ? Math.min(Number(range[2]), wav.length - 1) : wav.length - 1;
			return route.fulfill({ status: range ? 206 : 200, contentType: 'audio/wav', body: wav.subarray(start, end + 1),
				headers: { 'Accept-Ranges': 'bytes', 'Content-Length': String(end - start + 1),
					...(range ? { 'Content-Range': `bytes ${start}-${end}/${wav.length}` } : {}) } });
		});
		await page.goto('/library');
		await expect(page.getByTestId('library-item')).toHaveCount(2);
		const dawn = page.locator('[data-testid="library-item"][data-id="dawn"]');
		await dawn.getByTestId('track-more').click();
		await page.getByTestId('track-context-menu').getByRole('menuitem', { name: 'Add to queue', exact: true }).click();
		await page.locator('[data-testid="library-item"][data-id="coral"]').locator('.nt-library-row').click();
		await expect(page.getByTestId('audio-player')).toHaveCount(0);
		expect(requests).toEqual([]);
		expect(await page.evaluate(() => (window as unknown as { queuedPlayCalls: number }).queuedPlayCalls)).toBe(0);
		await page.evaluate(() => { (window as unknown as { teardownMarker: string }).teardownMarker = 'same-document'; });
		await navigate(page, 'home');
		await expect(page.getByRole('heading', { name: 'Library', exact: true })).toHaveCount(0);
		await expect(page.getByTestId('library-item')).toHaveCount(0);
		expect(await page.evaluate(() => (window as unknown as { teardownMarker: string }).teardownMarker)).toBe('same-document');
		await openQueue(page);
		const panel = page.getByTestId('queue-panel');
		const row = (id: string) => panel.locator(`[data-testid="queue-item"][data-track-id="${id}"]`);
		await expect(panel).toHaveCount(1);
		await expect(row('dawn')).toHaveAttribute('aria-current', 'true');
		await expect(page.getByTestId('audio-player')).toHaveCount(0);
		expect(requests).toEqual([]);
		await row('dawn').getByTestId('queue-more').click();
		await page.getByTestId('track-context-menu').getByRole('menuitem', { name: 'Play now', exact: true }).click();
		await expect(row('dawn')).toHaveAttribute('aria-current', 'true');
		await expect.poll(() => page.evaluate(() => (window as unknown as { queuedPlayCalls: number }).queuedPlayCalls))
			.toBe(outcome === 'no-stream' ? 0 : 1);
		const audio = page.getByTestId('audio-player');
		if (outcome === 'no-stream') {
			await expect(audio).toHaveCount(0);
			await expect(page.getByTestId('queue-announcement')).toHaveText('Selected Dawn Chorus; no stream to play.');
			expect(requests).toEqual([]);
		} else {
			await expect(audio).toHaveAttribute('src', '/test-audio/teardown-dawn');
			await expect(audio).toHaveJSProperty('readyState', 4);
			expect(requests.length).toBeGreaterThan(0);
			expect(requests.every((url) => url.endsWith('/teardown-dawn'))).toBe(true);
			if (outcome === 'blocked') {
				await expect(audio).toHaveJSProperty('paused', true);
				await expect(page.getByTestId('queue-announcement')).toHaveText('Selected Dawn Chorus, paused: playback was blocked.');
			} else {
				await expect(audio).toHaveJSProperty('paused', false);
				await expect.poll(() => audio.evaluate((element: HTMLAudioElement) => element.currentTime)).toBeGreaterThan(0.2);
				if (outcome === 'accepted') {
					await expect(page.getByTestId('queue-announcement')).toHaveText('Playing Dawn Chorus.');
				} else {
					await expect.poll(() => page.evaluate(() => typeof (window as unknown as { finishQueuedPlay?: () => void }).finishQueuedPlay)).toBe('function');
					await row('coral').getByRole('button', { name: 'Select Coral Night', exact: true }).click();
					const winning = await page.getByTestId('queue-announcement').textContent();
					expect(winning).toBe('Selected Coral Night');
					await page.evaluate(() => (window as unknown as { finishQueuedPlay: () => void }).finishQueuedPlay());
					await expect(page.getByTestId('queue-announcement')).toHaveText(winning!);
					await expect(row('coral')).toHaveAttribute('aria-current', 'true');
					await expect(audio).toHaveCount(0);
				}
			}
		}
		expect(await panel.getByTestId('queue-item').evaluateAll((rows) => rows.map((row) => row.getAttribute('data-track-id')))).toEqual(['dawn', 'coral']);
		expect(await page.evaluate(() => (window as unknown as { queuedPlayCalls: number }).queuedPlayCalls)).toBe(outcome === 'no-stream' ? 0 : 1);
		expect(errors).toEqual([]);
	});
}

test('queue-only Next from a metadata-only selection retains the playable track resume point', async ({ page }) => {
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(error.message));
	const items = [{ ...ITEMS[0], streamUrl: '' }, ITEMS[1]];
	await page.addInitScript(() => localStorage.setItem('nntune:resume:v1', '{"continuity-b":25}'));
	await page.route('**/api/library', (route) => route.fulfill({ json: items }));
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
	await expect(page.getByTestId('library-item')).toHaveCount(2);
	for (const item of items) {
		await page.locator(`[data-testid="library-item"][data-id="${item.id}"]`)
			.getByRole('button', { name: new RegExp(`^${item.title}( — |$)`) }).click();
	}
	await openQueue(page);
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
	expect(await page.evaluate(() => localStorage.getItem('nntune:resume:v1'))).toBe('{"continuity-b":25}');
	await page.getByTestId('queue-panel').getByTestId('player-next').click();
	const audio = page.getByTestId('audio-player');
	await expect(audio).toHaveCount(1);
	await expect(audio).toHaveAttribute('src', ITEMS[1].streamUrl);
	await expect(audio).toHaveJSProperty('readyState', 4);
	await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.seekable.length ? el.seekable.end(0) : 0)).toBeGreaterThan(25);
	await expect(audio).toHaveJSProperty('currentTime', 25);
	await expect(audio).toHaveJSProperty('paused', true);
	await expect(page.getByTestId('now-playing')).toContainText('Continuity B');
	expect(errors).toEqual([]);
});

for (const playing of [false, true]) {
	for (const refuseStorage of [false, true]) {
		test(`Favorites remain authoritative across navigation with ${refuseStorage ? 'refused' : 'working'} storage and the real ${playing ? 'playing' : 'paused'} decoder`, async ({ page }) => {
			const { audio, errors } = await openLibrary(page);
			await openQueue(page);
			const original = (await audio.elementHandle())!;
			await audio.evaluate((el: HTMLAudioElement) => {
				const events: string[] = [];
				for (const name of ['play', 'pause', 'loadstart', 'emptied']) el.addEventListener(name, () => events.push(name));
				(el as HTMLAudioElement & { favoriteEvents: string[] }).favoriteEvents = events;
			});
			if (!playing) {
				await togglePlayViaPlayerUi(page, audio);
				await expect.poll(() => audio.evaluate((el) => (el as HTMLAudioElement & { favoriteEvents: string[] }).favoriteEvents)).toEqual(['pause']);
				await audio.evaluate((el) => { (el as HTMLAudioElement & { favoriteEvents: string[] }).favoriteEvents.length = 0; });
			}
			if (refuseStorage) await page.evaluate(() => {
				const write = Storage.prototype.setItem;
				Storage.prototype.setItem = function (key, value) {
					if (key.startsWith('nntune.library.favorites.v1')) throw new DOMException('Refused', 'QuotaExceededError');
					return write.call(this, key, value);
				};
			});
			const libraryA = page.locator('[data-testid="library-item"][data-id="continuity-a"]');
			const queuedB = page.locator('[data-testid="queue-item"][data-track-id="continuity-b"]');
			await libraryA.getByRole('button', { name: 'Star Continuity A', exact: true }).click();
			const star = queuedB.getByRole('button', { name: 'Star Continuity B', exact: true });
			await star.focus();
			await star.press('Enter');
			await expect(queuedB.getByRole('button', { name: 'Unstar Continuity B', exact: true })).toBeFocused();
			await expect(page.getByTestId('queue-announcement')).toHaveText('Starred Continuity B');
			await expect(page.getByTestId('favorite-item')).toHaveCount(2);
			const before = await audio.evaluate((el: HTMLAudioElement) => el.currentTime);
			await navigate(page, 'home');
			await navigate(page, 'library');
			await expect(page.getByTestId('library-item')).toHaveCount(2);
			await expect.poll(() => page.getByTestId('favorite-item').evaluateAll((rows) => rows.map((row) => row.getAttribute('data-id')))).toEqual(['continuity-a', 'continuity-b']);
			await expect(libraryA.getByRole('button', { name: 'Unstar Continuity A', exact: true })).toHaveAttribute('aria-pressed', 'true');
			await expect(queuedB.getByRole('button', { name: 'Unstar Continuity B', exact: true })).toHaveAttribute('aria-pressed', 'true');
			await expect(page.getByTestId('queue-announcement')).toHaveText('Starred Continuity B');
			expect(await original.evaluate((el) => el === document.querySelector('[data-testid="audio-player"]'))).toBe(true);
			await expect(audio).toHaveCount(1);
			await expect(audio).toHaveAttribute('src', ITEMS[0].streamUrl);
			await expect(audio).toHaveJSProperty('paused', !playing);
			if (playing) await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBeGreaterThan(before);
			else expect(await audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBe(before);
			expect(await audio.evaluate((el) => (el as HTMLAudioElement & { favoriteEvents: string[] }).favoriteEvents)).toEqual([]);
			await queuedB.getByRole('button', { name: 'Unstar Continuity B', exact: true }).click();
			await expect(page.getByTestId('queue-announcement')).toHaveText('Unstarred Continuity B');
			await expect(page.getByTestId('favorite-item')).toHaveCount(1);
			await expect(libraryA.getByRole('button', { name: 'Unstar Continuity A', exact: true })).toHaveAttribute('aria-pressed', 'true');
			expect(errors).toEqual([]);
		});
	}
}

for (const refuseStorage of [false, true]) {
	test(`an away-route auth boundary purges ${refuseStorage ? 'memory-only' : 'persisted'} Favorites before the next account loads`, async ({ page }) => {
		const { audio, errors } = await openLibrary(page);
		await openQueue(page);
		const context = await page.evaluate(() => sessionStorage.getItem('nntune.library.auth-context.v1'));
		expect(context).not.toBeNull();
		if (refuseStorage) await page.evaluate(() => {
			const write = Storage.prototype.setItem;
			Storage.prototype.setItem = function (key, value) {
				if (key.startsWith('nntune.library.favorites.v1')) throw new DOMException('Refused', 'QuotaExceededError');
				return write.call(this, key, value);
			};
		});
		await page.locator('[data-testid="library-item"][data-id="continuity-a"]').getByRole('button', { name: 'Star Continuity A', exact: true }).click();
		await expect(page.getByTestId('favorite-item')).toHaveCount(1);
		await navigate(page, 'home');
		const other = await page.context().newPage();
		await other.goto('/login');
		await other.evaluate(() => {
			localStorage.setItem('nntune.library.auth-transition.v1', 'favorites-away-auth-change');
			localStorage.removeItem('nntune.library.auth-transition.v1');
		});
		await expect(audio).toHaveCount(0);
		expect(await page.evaluate((context) => localStorage.getItem(`nntune.library.favorites.v1.${context}`), context)).toBeNull();
		await other.close();
		await navigate(page, 'library');
		await expect(page.getByTestId('library-item')).toHaveCount(2);
		await expect(page.getByTestId('favorite-item')).toHaveCount(0);
		await expect(page.getByTestId('favorites-empty')).toBeVisible();
		await expect(page.locator('[data-testid="library-item"][data-id="continuity-a"]').getByRole('button', { name: 'Star Continuity A', exact: true })).toHaveAttribute('aria-pressed', 'false');
		await expect(page.getByTestId('queue-announcement')).toHaveText('');
		await expect(page.getByTestId('queue-item')).toHaveCount(0);
		expect(await page.evaluate(() => sessionStorage.getItem('nntune.library.auth-context.v1'))).not.toBe(context);
		expect(errors).toEqual([]);
	});
}

for (const firstLoad of ['pending', 'failed'] as const) {
	test(`auth purges stored Favorites before bootstrap when the first catalogue is ${firstLoad}`, async ({ page }) => {
		const context = 'favorites-prebootstrap-context';
		await page.addInitScript((context) => {
			if (!sessionStorage.getItem('favorites-prebootstrap-seeded')) {
				sessionStorage.setItem('favorites-prebootstrap-seeded', 'yes');
				sessionStorage.setItem('nntune.library.auth-context.v1', context);
				localStorage.setItem(`nntune.library.favorites.v1.${context}`, '["continuity-a"]');
			}
		}, context);
		let first = true;
		let pending: Route | null = null;
		await page.route('**/api/library', (route) => {
			if (!first) return route.fulfill({ json: ITEMS });
			if (firstLoad === 'pending') pending = route;
			else return route.fulfill({ status: 500, json: {} });
		});
		await page.goto('/library');
		await expect(page.locator('html')).toHaveAttribute('data-hydrated', '');
		if (firstLoad === 'pending') await expect.poll(() => pending !== null).toBe(true);
		else await expect(page.getByRole('button', { name: 'Retry', exact: true })).toBeVisible();
		const other = await page.context().newPage();
		await other.goto('/login');
		await other.evaluate(() => {
			localStorage.setItem('nntune.library.auth-transition.v1', 'favorites-before-bootstrap');
			localStorage.removeItem('nntune.library.auth-transition.v1');
		});
		await expect(page.getByRole('alert')).toContainText('Your session changed');
		expect(await page.evaluate((context) => localStorage.getItem(`nntune.library.favorites.v1.${context}`), context)).toBeNull();
		expect(await page.evaluate(() => sessionStorage.getItem('nntune.library.auth-context.v1'))).toBeNull();
		await other.close();
		first = false;
		if (pending) await (pending as Route).fulfill({ json: ITEMS });
		await page.getByRole('button', { name: 'Retry', exact: true }).click();
		await expect(page.getByTestId('library-item')).toHaveCount(2);
		await expect(page.getByTestId('favorite-item')).toHaveCount(0);
		await openQueue(page);
		await expect(page.getByTestId('queue-item')).toHaveCount(0);
		await expect(page.getByTestId('audio-player')).toHaveCount(0);
	});
}

type ExportWindow = Window & {
	exportWrites: string[];
	settleExport: (rejected: boolean) => void;
};

for (const playing of [false, true]) {
	for (const rejected of [false, true]) {
		for (const settleBeforeNavigation of [false, true]) {
			test(`whole-queue export ${rejected ? 'fallback' : 'success'} survives ${settleBeforeNavigation ? 'completed' : 'pending'} navigation with the real ${playing ? 'playing' : 'paused'} decoder`, async ({ page }) => {
				const { audio, errors } = await openLibrary(page);
				await openQueue(page);
				const original = (await audio.elementHandle())!;
				await audio.evaluate((el: HTMLAudioElement) => {
					const events: string[] = [];
					for (const name of ['play', 'pause', 'loadstart', 'emptied']) el.addEventListener(name, () => events.push(name));
					(el as HTMLAudioElement & { viewEvents: string[] }).viewEvents = events;
				});
				if (!playing) {
					await togglePlayViaPlayerUi(page, audio);
					await expect.poll(() => audio.evaluate((el) => (el as HTMLAudioElement & { viewEvents: string[] }).viewEvents)).toEqual(['pause']);
				}
				const before = await audio.evaluate((el: HTMLAudioElement) => {
					(el as HTMLAudioElement & { viewEvents: string[] }).viewEvents.length = 0;
					return el.currentTime;
				});
				await page.evaluate(() => {
					const probe = window as ExportWindow;
					probe.exportWrites = [];
					Object.defineProperty(navigator, 'clipboard', { configurable: true, value: {
						writeText: (text: string) => new Promise<void>((resolve, reject) => {
							probe.exportWrites.push(text);
							probe.settleExport = (rejected) => rejected ? reject(new Error('Denied')) : resolve();
						})
					} });
				});
				await page.getByRole('searchbox', { name: 'Filter queue', exact: true }).fill('Continuity B');
				await expect(page.getByTestId('queue-item')).toHaveCount(1);
				const copy = page.getByRole('button', { name: 'Copy queue as text', exact: true });
				await copy.focus();
				await copy.press('Enter');
				await expect(copy).toBeFocused();
				const text = '1. Continuity A — Test artist\n2. Continuity B — Test artist';
				await expect.poll(() => page.evaluate(() => (window as ExportWindow).exportWrites)).toEqual([text]);
				if (settleBeforeNavigation) {
					await page.evaluate((rejected) => (window as ExportWindow).settleExport(rejected), rejected);
					await expect(page.getByTestId('queue-export-status')).toHaveText(rejected ? 'Clipboard unavailable. Copy the queue manually below.' : 'Copied queue as text.');
				}
				await navigate(page, 'home');
				await expect(page.getByRole('main').getByTestId('queue-export-status')).toHaveCount(0);
				await expect(page.getByTestId('queue-panel').getByTestId('queue-export-status')).toHaveCount(1);
				if (!settleBeforeNavigation) await page.evaluate((rejected) => (window as ExportWindow).settleExport(rejected), rejected);
				await navigate(page, 'library');
				await expect(page.getByTestId('library-item')).toHaveCount(2);
				await expect(page.getByTestId('queue-export-status')).toHaveCount(1);
				await expect(page.getByTestId('queue-export-status')).toHaveText(rejected ? 'Clipboard unavailable. Copy the queue manually below.' : 'Copied queue as text.');
				if (rejected) {
					const fallback = page.getByTestId('queue-export-fallback');
					await expect(fallback).toHaveValue(text);
					await expect(fallback).toHaveAccessibleName('Queue as text');
					await expect(fallback).toHaveJSProperty('readOnly', true);
				} else await expect(page.getByTestId('queue-export-fallback')).toHaveCount(0);
				await expect(page.getByRole('searchbox', { name: 'Filter queue', exact: true })).toHaveValue('Continuity B');
				expect(await original.evaluate((el) => el === document.querySelector('[data-testid="audio-player"]'))).toBe(true);
				await expect(audio).toHaveCount(1);
				await expect(audio).toHaveAttribute('src', ITEMS[0].streamUrl);
				await expect(audio).toHaveJSProperty('paused', !playing);
				if (playing) await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBeGreaterThan(before);
				else expect(await audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBe(before);
				expect(await audio.evaluate((el) => (el as HTMLAudioElement & { viewEvents: string[] }).viewEvents)).toEqual([]);
				expect(await page.evaluate(() => (window as ExportWindow).exportWrites)).toEqual([text]);
				expect(errors).toEqual([]);
			});
		}
	}
}

test('the same playing element and queue survive app navigation and a pending library refresh', async ({ page }) => {
	const { audio, errors, hold, release } = await openLibrary(page);
	const original = await audio.elementHandle();
	expect(original).not.toBeNull();
	await audio.evaluate((el: HTMLAudioElement) => {
		el.volume = 0.4;
		el.playbackRate = 1.5;
	});
	const before = await audio.evaluate((el: HTMLAudioElement) => el.currentTime);
	await navigate(page, 'home');
	await expect(page.getByRole('heading', { name: 'Home', exact: true })).toBeVisible();
	expect(await original!.evaluate((el) => el === document.querySelector('[data-testid="audio-player"]'))).toBe(true);
	await expect(audio).toHaveJSProperty('paused', false);
	await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBeGreaterThan(before);
	await expect(audio).toHaveJSProperty('volume', 0.4);
	await expect(audio).toHaveJSProperty('playbackRate', 1.5);

	hold();
	await navigate(page, 'library');
	// The route hides its queue panel during initial loading, but the selected
	// identity and shell audio must remain live before that refresh resolves.
	await expect(page.getByText('Loading your library…', { exact: true })).toBeVisible();
	await expect(page.getByTestId('library-now-playing')).toContainText('Continuity A');
	await expect(audio).toHaveJSProperty('paused', false);
	expect(await original!.evaluate((el) => el === document.querySelector('[data-testid="audio-player"]'))).toBe(true);
	await release();
	await expect(page.getByTestId('library-item')).toHaveCount(2);
	await openQueue(page);
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	await expect(page.getByTestId('now-playing')).toContainText('Continuity A');
	await page.screenshot({ path: '/tmp/two-1587-player-navigation.png', fullPage: true });
	expect(errors).toEqual([]);
});

for (const action of ['play-next', 'shuffle'] as const) {
	for (const playing of [false, true]) {
		test(`${action} preserves the real ${playing ? 'playing' : 'paused'} source, keyboard focus and arranged queue across navigation`, async ({ page }) => {
			const items = [...ITEMS, ...['c', 'd'].map((id) => ({
				id: `continuity-${id}`, title: `Continuity ${id.toUpperCase()}`, artist: 'Test artist', streamUrl: `/test-audio/continuity-${id}`
			}))];
			const { audio, errors } = await openLibrary(page, items);
			await openQueue(page);
			await page.getByTestId('player-next').click();
			await expect(audio).toHaveAttribute('src', ITEMS[1].streamUrl);
			await expect(audio).toHaveJSProperty('paused', false);
			await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBeGreaterThan(0.2);
			if (!playing) await togglePlayViaPlayerUi(page, audio);
			const original = (await audio.elementHandle())!;
			const row = page.locator('[data-testid="queue-item"][data-track-id="continuity-d"]');
			await row.getByRole('button', { name: 'Move Continuity D up', exact: true }).click();
			await row.getByRole('button', { name: 'Move Continuity D down', exact: true }).click();
			await expect(page.getByTestId('queue-announcement')).toHaveText('Continuity D moved to 4 of 4');
			const before = await audio.evaluate((el: HTMLAudioElement) => el.currentTime);
			if (action === 'shuffle') await page.evaluate(() => { Math.random = () => 0; });
			const control = action === 'shuffle'
				? page.getByRole('button', { name: 'Shuffle upcoming', exact: true })
				: row.getByRole('button', { name: 'Play Continuity D next', exact: true });
			await control.focus();
			await control.press('Enter');
			await expect(control).toBeFocused();
			const order = ['continuity-a', 'continuity-b', 'continuity-d', 'continuity-c'];
			await expect.poll(() => page.getByTestId('queue-item').evaluateAll((rows) => rows.map((el) => el.getAttribute('data-track-id')))).toEqual(order);
			await expect(page.locator('[data-testid="queue-item"][aria-current="true"]')).toHaveAttribute('data-track-id', 'continuity-b');
			await expect(page.getByTestId('queue-announcement')).toHaveText('');
			await navigate(page, 'home');
			expect(await original.evaluate((el) => el === document.querySelector('[data-testid="audio-player"]'))).toBe(true);
			await expect(audio).toHaveJSProperty('paused', !playing);
			if (playing) await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBeGreaterThan(before);
			else expect(await audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBe(before);
			await navigate(page, 'library');
			await expect(page.getByTestId('library-item')).toHaveCount(items.length);
			await expect.poll(() => page.getByTestId('queue-item').evaluateAll((rows) => rows.map((el) => el.getAttribute('data-track-id')))).toEqual(order);
			expect(await original.evaluate((el) => el === document.querySelector('[data-testid="audio-player"]'))).toBe(true);
			await expect(audio).toHaveJSProperty('paused', !playing);
			expect(errors).toEqual([]);
		});
	}
}

test('an auth transition tears down playback while the library route is absent', async ({ page }) => {
	const { audio, errors } = await openLibrary(page);
	const original = await audio.elementHandle();
	expect(original).not.toBeNull();
	await navigate(page, 'home');
	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate(() => {
		localStorage.setItem('nntune.library.auth-transition.v1', 'session-builder-change');
		localStorage.removeItem('nntune.library.auth-transition.v1');
	});
	await expect(audio).toHaveCount(0);
	expect(await original!.evaluate((el: HTMLAudioElement) => el.paused)).toBe(true);
	await otherTab.close();
	await navigate(page, 'library');
	await expect(page.getByTestId('library-item')).toHaveCount(2);
	await openQueue(page);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(audio).toHaveCount(0);
	expect(errors).toEqual([]);
});

for (const playing of [false, true]) {
	test(`queue filter survives client navigation without changing the real ${playing ? 'playing' : 'paused'} source or decoder`, async ({ page }) => {
		const { audio, errors } = await openLibrary(page);
		await openQueue(page);
		const original = (await audio.elementHandle())!;
		await audio.evaluate((el: HTMLAudioElement) => {
			const events: string[] = [];
			for (const name of ['play', 'pause', 'loadstart', 'emptied']) el.addEventListener(name, () => events.push(name));
			(el as HTMLAudioElement & { viewEvents: string[] }).viewEvents = events;
		});
		if (!playing) {
			await togglePlayViaPlayerUi(page, audio);
			// Native pause is queued separately from the synchronous paused flag.
			await expect.poll(() => audio.evaluate((el) => (el as HTMLAudioElement & { viewEvents: string[] }).viewEvents)).toEqual(['pause']);
		}
		const before = await audio.evaluate((el: HTMLAudioElement) => {
			(el as HTMLAudioElement & { viewEvents: string[] }).viewEvents.length = 0;
			return el.currentTime;
		});
		await page.getByRole('searchbox', { name: 'Filter queue', exact: true }).fill('  Continuity B  ');
		await expect(page.getByTestId('queue-item')).toHaveCount(1);
		await expect(page.getByTestId('queue-item')).toHaveAttribute('data-track-id', ITEMS[1].id);
		await navigate(page, 'home');
		await navigate(page, 'library');
		await expect(page.getByTestId('library-item')).toHaveCount(2);
		await expect(page.getByRole('searchbox', { name: 'Filter queue', exact: true })).toHaveValue('  Continuity B  ');
		await expect(page.getByTestId('queue-item')).toHaveCount(1);
		await expect(page.getByTestId('queue-item')).toHaveAttribute('data-track-id', ITEMS[1].id);
		expect(await original.evaluate((el) => el === document.querySelector('[data-testid="audio-player"]'))).toBe(true);
		await expect(audio).toHaveAttribute('src', ITEMS[0].streamUrl);
		await expect(audio).toHaveJSProperty('paused', !playing);
		if (playing) await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBeGreaterThan(before);
		else expect(await audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBe(before);
		const jump = page.getByRole('button', { name: 'Show current track', exact: true });
		await jump.focus();
		await jump.press('Enter');
		await expect(page.getByRole('searchbox', { name: 'Filter queue', exact: true })).toHaveValue('');
		await expect(page.getByTestId('queue-item')).toHaveCount(2);
		await expect(page.locator('[data-testid="queue-item"][aria-current="true"]')).toBeFocused();
		expect(await audio.evaluate((el) => (el as HTMLAudioElement & { viewEvents: string[] }).viewEvents)).toEqual([]);
		expect(errors).toEqual([]);
	});
}

test('leaving an in-flight refresh releases routine status suspension without stopping playback', async ({ page }) => {
	const { audio, errors, hold, release } = await openLibrary(page);
	const status = page.getByTestId('queue-announcement');
	const identity = page.getByTestId('now-playing');
	const original = (await audio.elementHandle())!;
	hold();
	try {
		await page.getByRole('button', { name: 'Refresh', exact: true }).click();
		await expect(page.getByText('Loading your library…', { exact: true })).toBeVisible();
		await expect(status).not.toHaveAttribute('role', 'status');
		await expect(status).not.toHaveAttribute('aria-live', 'polite');
		await expect(identity).not.toHaveAttribute('role', 'status');
		await navigate(page, 'home');
		await expect(status).toHaveAttribute('role', 'status');
		await expect(status).toHaveAttribute('aria-live', 'polite');
		await expect(identity).toHaveAttribute('role', 'status');
		await release();
		await expect(status).toHaveAttribute('role', 'status');
		expect(await original.evaluate((el) => el === document.querySelector('[data-testid="audio-player"]'))).toBe(true);
		await expect(audio).toHaveJSProperty('paused', false);
	} finally {
		await release();
	}
	expect(errors).toEqual([]);
});

test('route remount keeps the queue filter but never restores destructive consent', async ({ page }) => {
	const { errors } = await openLibrary(page);
	await openQueue(page);
	await page.getByRole('searchbox', { name: 'Filter queue', exact: true }).fill('Continuity B');
	await page.getByRole('button', { name: 'Clear queue', exact: true }).click();
	await expect(page.getByTestId('queue-clear-confirm')).toBeVisible();
	await navigate(page, 'home');
	await navigate(page, 'library');
	await expect(page.getByTestId('library-item')).toHaveCount(2);
	await expect(page.getByRole('searchbox', { name: 'Filter queue', exact: true })).toHaveValue('Continuity B');
	await expect(page.getByTestId('queue-clear-confirm')).toHaveCount(0);
	await page.getByRole('button', { name: 'Clear queue filter', exact: true }).click();
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	expect(errors).toEqual([]);
});

test('a cross-tab auth boundary clears session filter text while Library is unmounted', async ({ page }) => {
	const { audio, errors } = await openLibrary(page);
	await openQueue(page);
	await page.getByRole('searchbox', { name: 'Filter queue', exact: true }).fill('Previous account query');
	await navigate(page, 'home');
	const other = await page.context().newPage();
	await other.goto('/login');
	await other.evaluate(() => {
		localStorage.setItem('nntune.library.auth-transition.v1', 'queue-filter-auth-change');
		localStorage.removeItem('nntune.library.auth-transition.v1');
	});
	await expect(audio).toHaveCount(0);
	await other.close();
	await navigate(page, 'library');
	await expect(page.getByTestId('library-item')).toHaveCount(2);
	await expect(page.getByRole('searchbox', { name: 'Filter queue', exact: true })).toHaveValue('');
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	expect(errors).toEqual([]);
});

type QueueCopyControl = { writes: { url: string; resolve: () => void; reject: () => void }[] };

async function deferQueueCopies(page: Page) {
	await page.evaluate(() => {
		const control: QueueCopyControl = { writes: [] };
		(window as unknown as { __queueCopies: QueueCopyControl }).__queueCopies = control;
		Object.defineProperty(navigator, 'clipboard', { configurable: true, value: {
			writeText: (url: string) => new Promise<void>((resolve, reject) => {
				control.writes.push({ url, resolve, reject: () => reject(new Error('Clipboard refused')) });
			})
		} });
	});
}

async function finishQueueCopy(page: Page, index: number, reject: boolean) {
	await page.evaluate(({ index, reject }) => {
		const control = (window as unknown as { __queueCopies: QueueCopyControl }).__queueCopies;
		const write = control.writes[index];
		if (!write) throw new Error('No pending queue copy');
		if (reject) write.reject();
		else write.resolve();
	}, { index, reject });
}

for (const reject of [false, true]) {
	for (const auth of [false, true]) {
		test(`queued row copy ${reject ? 'fallback' : 'success'} survives navigation but ${auth ? 'not an auth boundary' : 'keeps the live session'}`, async ({ page }) => {
			const { audio, errors } = await openLibrary(page);
			await openQueue(page);
			const original = (await audio.elementHandle())!;
			await deferQueueCopies(page);
			const status = page.getByTestId('queue-share-status');
			const region = (await status.elementHandle())!;
			await page.locator('[data-testid="queue-item"][data-track-id="continuity-b"]').getByTestId('queue-copy-link').click();
			await navigate(page, 'home');
			await expect(status).toHaveCount(1);
			expect(await region.evaluate((el) => el === document.querySelector('[data-testid="queue-share-status"]'))).toBe(true);
			if (auth) {
				const other = await page.context().newPage();
				await other.goto('/login');
				await other.evaluate(() => {
					localStorage.setItem('nntune.library.auth-transition.v1', 'queue-copy-auth-change');
					localStorage.removeItem('nntune.library.auth-transition.v1');
				});
				await expect(audio).toHaveCount(0);
				await other.close();
			}
			await finishQueueCopy(page, 0, reject);
			if (auth) {
				await expect(status).toHaveText('');
				await expect(page.getByTestId('queue-share-fallback')).toHaveCount(0);
			} else {
				if (reject) {
					await expect(page.getByTestId('queue-share-fallback')).toHaveValue(`${new URL(page.url()).origin}/library?track=continuity-b&t=0`);
					await expect(page.getByTestId('queue-share-instructions')).toBeVisible();
				} else await expect(status).toHaveText('Copied link for Continuity B.');
				expect(await same(original)).toBe(true);
				await expect(audio).toHaveJSProperty('paused', false);
			}
			await navigate(page, 'library');
			await expect(page.getByTestId('library-item')).toHaveCount(2);
			if (auth) {
				await expect(status).toHaveText('');
				await expect(page.getByTestId('queue-share-fallback')).toHaveCount(0);
			} else {
				if (reject) await expect(page.getByTestId('queue-share-fallback')).toBeVisible();
				else await expect(status).toHaveText('Copied link for Continuity B.');
				await page.getByRole('button', { name: 'Remove Continuity B from queue', exact: true }).click();
				await expect(status).toHaveText('');
				await expect(page.getByTestId('queue-share-fallback')).toHaveCount(0);
			}
			expect(errors).toEqual([]);
		});
	}
}

test('queued copies read the live decoder clock and retain latest intent after navigation', async ({ page }) => {
	const { audio, errors } = await openLibrary(page);
	await openQueue(page);
	await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBeGreaterThan(1.2);
	await deferQueueCopies(page);
	const before = await audio.evaluate((el: HTMLAudioElement) => el.currentTime);
	await page.locator('[data-testid="queue-item"][data-track-id="continuity-a"]').getByTestId('queue-copy-link').click();
	await page.locator('[data-testid="queue-item"][data-track-id="continuity-b"]').getByTestId('queue-copy-link').click();
	const urls = await page.evaluate(() => (window as unknown as { __queueCopies: QueueCopyControl }).__queueCopies.writes.map((write) => write.url));
	expect(Number(new URL(urls[0]).searchParams.get('t'))).toBeGreaterThanOrEqual(Math.floor(before));
	expect(Number(new URL(urls[0]).searchParams.get('t'))).toBeLessThanOrEqual(Math.floor(await audio.evaluate((el: HTMLAudioElement) => el.currentTime)));
	expect(new URL(urls[1]).searchParams.get('t')).toBe('0');
	await navigate(page, 'home');
	await finishQueueCopy(page, 1, false);
	await expect(page.getByTestId('queue-share-status')).toHaveText('Copied link for Continuity B.');
	await finishQueueCopy(page, 0, true);
	await expect(page.getByTestId('queue-share-status')).toHaveText('Copied link for Continuity B.');
	await expect(page.getByTestId('queue-share-fallback')).toHaveCount(0);
	expect(errors).toEqual([]);
});

for (const width of [320, 390, 1280]) {
	test(`persistent queue-copy fallback remains labelled and contained at ${width}px`, async ({ page }) => {
		await page.setViewportSize({ width, height: 900 });
		const { errors } = await openLibrary(page);
		await openQueue(page);
		await page.evaluate(() => Object.defineProperty(navigator, 'clipboard', { configurable: true, value: undefined }));
		await page.locator('[data-testid="queue-item"][data-track-id="continuity-b"]').getByTestId('queue-copy-link').click();
		await navigate(page, 'home');
		const field = page.getByRole('textbox', { name: 'Queued track link', exact: true });
		await expect(field).toHaveAttribute('readonly', '');
		await expect(field).toHaveAttribute('aria-describedby', 'queue-share-instructions');
		await field.focus();
		await expect(field).toBeFocused();
		expect(await field.evaluate((el) => {
			const r = el.getBoundingClientRect();
			return r.left >= 0 && r.right <= window.innerWidth && document.documentElement.scrollWidth <= window.innerWidth;
		})).toBe(true);
		const blocking = (await new AxeBuilder({ page }).analyze()).violations.filter((v) => v.impact === 'serious' || v.impact === 'critical');
		expect(blocking).toEqual([]);
		expect(errors).toEqual([]);
	});
}

const same = (handle: NonNullable<Awaited<ReturnType<Locator['elementHandle']>>>) =>
	handle.evaluate((el) => el === document.querySelector('[data-testid="audio-player"]'));

for (const playing of [true, false]) {
	test(`session queue edits preserve the real ${playing ? 'playing' : 'paused'} source and feedback across navigation`, async ({ page }) => {
		const { audio, errors } = await openLibrary(page);
		await openQueue(page);
		if (!playing) await togglePlayViaPlayerUi(page, audio);
		const original = (await audio.elementHandle())!;
		const before = await audio.evaluate((el: HTMLAudioElement) => el.currentTime);
		const feedback = page.getByTestId('queue-announcement');
		const region = (await feedback.elementHandle())!;
		await page.getByRole('button', { name: 'Move Continuity B up', exact: true }).click();
		await expect(feedback).toHaveText('Continuity B moved to 1 of 2');
		await expect(page.getByTestId('queue-item').filter({ has: page.getByRole('button', { name: 'Select Continuity A', exact: true }) })).toHaveAttribute('aria-current', 'true');
		expect(await same(original)).toBe(true);
		await expect(audio).toHaveJSProperty('paused', !playing);
		await page.getByRole('button', { name: 'Remove Continuity B from queue', exact: true }).click();
		await expect(feedback).toHaveText('Continuity B removed from the queue.');
		await page.getByRole('button', { name: 'Select Continuity A', exact: true }).click();
		await expect(feedback).toHaveText('Continuity B removed from the queue.');
		await navigate(page, 'home');
		expect(await same(original)).toBe(true);
		expect(await region.evaluate((el) => el === document.querySelector('[data-testid="queue-announcement"]'))).toBe(true);
		await expect(audio).toHaveJSProperty('paused', !playing);
		if (playing) await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBeGreaterThan(before);
		else expect(await audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBe(before);
		await navigate(page, 'library');
		await expect(page.getByTestId('queue-item')).toHaveCount(1);
		await expect(feedback).toHaveText('Continuity B removed from the queue.');
		expect(errors).toEqual([]);
	});

	test(`direct queue selection and current removal never inherit ${playing ? 'playing' : 'paused'} autoplay intent`, async ({ page }) => {
		const { audio, errors } = await openLibrary(page);
		await openQueue(page);
		if (!playing) await togglePlayViaPlayerUi(page, audio);
		const original = (await audio.elementHandle())!;
		await page.getByRole('button', { name: 'Select Continuity B', exact: true }).click();
		await expect(audio).toHaveAttribute('src', ITEMS[1].streamUrl);
		await expect(audio).toHaveJSProperty('readyState', 4);
		await expect(audio).toHaveJSProperty('paused', true);
		expect(await same(original)).toBe(false);
		expect(await original.evaluate((el: HTMLAudioElement) => el.paused)).toBe(true);
		await expect(page.getByTestId('queue-announcement')).toHaveText('Selected Continuity B');
		await page.getByRole('button', { name: 'Remove Continuity B from queue', exact: true }).click();
		await expect(audio).toHaveAttribute('src', ITEMS[0].streamUrl);
		await expect(audio).toHaveJSProperty('readyState', 4);
		await expect(audio).toHaveJSProperty('paused', true);
		await navigate(page, 'home');
		await expect(audio).toHaveJSProperty('paused', true);
		await expect(page.getByTestId('now-playing')).toContainText('Continuity A');
		await expect(page.getByTestId('queue-announcement')).toHaveText('Continuity B removed from the queue.');
		expect(errors).toEqual([]);
	});
}

for (const playing of [true, false]) {
	test(`system navigation away from the library preserves ${playing ? 'playing' : 'paused'} intent on real media`, async ({ page }) => {
		await page.addInitScript(() => {
			// Capture the actual production callbacks, still registering them with
			// the browser. Do not replace the media element or synthesize its events.
			const handlers = new Map<MediaSessionAction, MediaSessionActionHandler>();
			const native = navigator.mediaSession.setActionHandler.bind(navigator.mediaSession);
			navigator.mediaSession.setActionHandler = (action, handler) => {
				if (handler) handlers.set(action, handler);
				else handlers.delete(action);
				return native(action, handler);
			};
			Object.assign(window, {
				__invokeSystemAction: (action: MediaSessionAction) => {
					const handler = handlers.get(action);
					if (!handler) throw new Error(`No production handler for ${action}`);
					handler({ action });
				}
			});
			localStorage.setItem('nntune:resume:v1', '{"continuity-b":25}');
		});
		const { audio, errors } = await openLibrary(page);
		if (!playing) {
			await togglePlayViaPlayerUi(page, audio);
			await expect(audio).toHaveJSProperty('paused', true);
		}
		await navigate(page, 'home');
		const first = (await audio.elementHandle())!;
		const invoke = (action: 'nexttrack' | 'previoustrack') => page.evaluate((name) => {
			(window as unknown as { __invokeSystemAction: (action: string) => void }).__invokeSystemAction(name);
		}, action);

		await invoke('nexttrack');
		await expect(audio).toHaveAttribute('src', ITEMS[1].streamUrl);
		await expect(audio).toHaveJSProperty('readyState', 4);
		await expect(audio).toHaveJSProperty('paused', !playing);
		expect(await same(first)).toBe(false);
		expect(await first.evaluate((el: HTMLAudioElement) => el.paused)).toBe(true);
		// Deliberate navigation must not restore B's saved 25-second position.
		expect(await audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBeLessThan(3);
		if (playing) {
			await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBeGreaterThan(0.1);
		}

		await invoke('previoustrack');
		await expect(audio).toHaveAttribute('src', ITEMS[0].streamUrl);
		await expect(audio).toHaveJSProperty('readyState', 4);
		await expect(audio).toHaveJSProperty('paused', !playing);
		expect(await audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBeLessThan(3);
		await expect(page.locator('audio')).toHaveCount(1);
		await navigate(page, 'library');
		await expect(page.getByTestId('now-playing')).toContainText('Continuity A');
		await openQueue(page);
		await expect(page.getByTestId('queue-item')).toHaveCount(2);
		await expect(audio).toHaveJSProperty('paused', !playing);
		expect(errors).toEqual([]);
	});
}

test('the same paused element, queue and position survive app navigation with exactly one audio node', async ({ page }) => {
	const { audio, errors, hold, release } = await openLibrary(page);
	const original = (await audio.elementHandle())!;
	await togglePlayViaPlayerUi(page, audio);
	await expect(audio).toHaveJSProperty('paused', true);
	const position = await audio.evaluate((el: HTMLAudioElement) => el.currentTime);
	expect(position).toBeGreaterThan(0.2);
	await expect(page.getByTestId('audio-player')).toHaveCount(1);

	await navigate(page, 'home');
	await expect(page.getByRole('heading', { name: 'Home', exact: true })).toBeVisible();
	await expect(page.getByTestId('audio-player')).toHaveCount(1);
	expect(await same(original)).toBe(true);
	await expect(audio).toHaveJSProperty('paused', true);
	await page.waitForTimeout(600);
	expect(await audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBe(position);

	hold();
	await navigate(page, 'library');
	await expect(page.getByText('Loading your library…', { exact: true })).toBeVisible();
	await expect(page.getByTestId('audio-player')).toHaveCount(1);
	await expect(page.getByTestId('library-now-playing')).toContainText('Continuity A');
	expect(await same(original)).toBe(true);
	await release();
	await openQueue(page);
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	await expect(page.getByTestId('now-playing')).toContainText('Continuity A');
	await expect(page.getByTestId('audio-player')).toHaveCount(1);
	expect(await same(original)).toBe(true);
	await expect(audio).toHaveJSProperty('paused', true);
	expect(await audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBe(position);
	expect(errors).toEqual([]);
});

for (const failure of [500, 'network'] as const) {
	test(`returning to the library through a ${failure} failure keeps the playing audio and queue, then Retry succeeds`, async ({ page }) => {
		const { audio, errors, fail } = await openLibrary(page);
		const original = (await audio.elementHandle())!;
		await navigate(page, 'home');
		fail(failure);
		await navigate(page, 'library');
		await expect(page.getByRole('alert')).toHaveText('Library is not reachable right now.');
		await expect(page.getByTestId('audio-player')).toHaveCount(1);
		expect(await same(original)).toBe(true);
		await expect(audio).toHaveJSProperty('paused', false);
		await expect(page.getByTestId('library-now-playing')).toContainText('Continuity A');

		fail(null);
		await page.getByRole('button', { name: 'Retry', exact: true }).click();
		await expect(page.getByTestId('library-item')).toHaveCount(2);
		await openQueue(page);
		await expect(page.getByTestId('queue-item')).toHaveCount(2);
		await expect(page.getByTestId('now-playing')).toContainText('Continuity A');
		await expect(page.getByTestId('audio-player')).toHaveCount(1);
		expect(await same(original)).toBe(true);
		await expect(audio).toHaveJSProperty('paused', false);
		expect(errors).toEqual([]);
	});
}

for (const status of [401, 403, 404] as const) {
	test(`returning to the library with HTTP ${status} clears the queue and audio`, async ({ page }) => {
		const { audio, errors, fail } = await openLibrary(page);
		await navigate(page, 'home');
		fail(status);
		await navigate(page, 'library');
		await expect(page.getByTestId('library-item')).toHaveCount(0);
		await expect(audio).toHaveCount(0);
		await openQueue(page);
		await expect(page.getByTestId('queue-item')).toHaveCount(0);
		fail(null);
		const retry = page.getByRole('button', { name: 'Retry', exact: true });
		if (status === 404) {
			await navigate(page, 'home');
			await navigate(page, 'library');
		} else {
			await retry.click();
		}
		await expect(page.getByTestId('library-item')).toHaveCount(2);
		await expect(page.getByTestId('queue-item')).toHaveCount(0);
		await expect(audio).toHaveCount(0);
		expect(errors).toEqual([]);
	});
}

async function openRows(page: Page, items: Record<string, unknown>[]) {
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(error.message));
	await page.route('**/api/library', (route) => route.fulfill({ json: items }));
	const wav = silentWav();
	await page.route('**/test-audio/**', (route) => route.fulfill({ contentType: 'audio/wav', body: wav }));
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(items.length);
	return errors;
}

const BAD_URLS: [string, Record<string, unknown>][] = [
	['missing', {}],
	['null', { streamUrl: null }],
	['blank', { streamUrl: '' }],
	['whitespace', { streamUrl: ' \t  ' }],
	['non-string', { streamUrl: 7 }]
];

for (const [label, extra] of BAD_URLS) {
	test(`a ${label} streamUrl selects the row without mounting audio`, async ({ page }) => {
		const errors = await openRows(page, [{ id: 'bad-url', title: 'Bad URL', artist: 'Test artist', ...extra }]);
		await page.getByTestId('library-item').getByRole('button', { name: /^Bad URL( — |$)/ }).click();
		await openQueue(page);
		await expect(page.getByTestId('queue-item')).toHaveCount(1);
		await expect(page.getByTestId('audio-player')).toHaveCount(0);
		await page.waitForTimeout(300);
		await expect(page.getByTestId('audio-player')).toHaveCount(0);
		expect(await page.locator('audio').count()).toBe(0);
		expect(errors).toEqual([]);
	});
}

test('a padded valid streamUrl is trimmed to the exact src and decodes', async ({ page }) => {
	const errors = await openRows(page, [
		{ id: 'padded', title: 'Padded', artist: 'Test artist', streamUrl: '  \t/test-audio/padded \n ' }
	]);
	await page.getByTestId('library-item').getByRole('button', { name: /^Padded( — |$)/ }).click();
	const audio = page.getByTestId('audio-player');
	await expect(audio).toHaveCount(1);
	await expect(audio).toHaveAttribute('src', '/test-audio/padded');
	await expect(audio).toHaveJSProperty('readyState', 4);
	await togglePlayViaPlayerUi(page, audio);
	await expect(audio).toHaveJSProperty('paused', false);
	await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBeGreaterThan(0.2);
	expect(errors).toEqual([]);
});

for (const boundary of ['next', 'auth'] as const) {
	for (const outcome of ['resolve', 'reject'] as const) {
		test(`a pending route Play ${outcome} after unmount and ${boundary} never publishes obsolete global feedback`, async ({ page }) => {
			await page.addInitScript(() => {
				const w = window as unknown as { __settleRoutePlay?: (reject: boolean) => void };
				HTMLMediaElement.prototype.play = function () {
					return new Promise<void>((resolve, reject) => {
						w.__settleRoutePlay = (failed) => failed ? reject(new DOMException('blocked', 'NotAllowedError')) : resolve();
					});
				};
			});
			const errors = await openRows(page, ITEMS);
			await page.getByRole('button', { name: 'Play displayed from the top' }).click();
			await expect.poll(() => page.evaluate(() => typeof (window as unknown as { __settleRoutePlay?: unknown }).__settleRoutePlay)).toBe('function');
			await navigate(page, 'home');
			if (boundary === 'next') {
				await page.getByTestId('player-next').click();
				await expect(page.getByTestId('audio-player')).toHaveAttribute('src', ITEMS[1].streamUrl);
			} else {
				const otherTab = await page.context().newPage();
				await otherTab.goto('/login');
				await otherTab.evaluate(() => {
					localStorage.setItem('nntune.library.auth-transition.v1', 'unmounted-play-change');
					localStorage.removeItem('nntune.library.auth-transition.v1');
				});
				await otherTab.close();
				await expect(page.getByTestId('audio-player')).toHaveCount(0);
			}
			await page.evaluate((failed) => {
				(window as unknown as { __settleRoutePlay: (reject: boolean) => void }).__settleRoutePlay(failed);
			}, outcome === 'reject');
			await page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
			expect((await page.getByTestId('queue-announcement').allTextContents()).map((text) => text.trim()).filter(Boolean)).toEqual([]);
			expect(errors).toEqual([]);
		});
	}
}

test('a pending Play rejected after a genuine auth transition never restores the old title or announcement', async ({ page }) => {
	await page.addInitScript(() => {
		const w = window as unknown as { __holdPlay: boolean; __reject: ((e: unknown) => void)[] };
		w.__holdPlay = false;
		w.__reject = [];
		const original = HTMLMediaElement.prototype.play;
		HTMLMediaElement.prototype.play = function (this: HTMLMediaElement) {
			if (!w.__holdPlay) return original.call(this);
			return new Promise<void>((_, reject) => w.__reject.push(reject));
		};
	});
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(error.message));
	await page.route('**/api/library', (route) => route.fulfill({ json: ITEMS }));
	const wav = silentWav();
	await page.route('**/test-audio/**', (route) => route.fulfill({ contentType: 'audio/wav', body: wav }));
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(2);
	await page.evaluate(() => { (window as unknown as { __holdPlay: boolean }).__holdPlay = true; });
	await page.getByRole('button', { name: 'Play displayed from the top' }).click();
	await expect.poll(() => page.evaluate(() => (window as unknown as { __reject: unknown[] }).__reject.length)).toBe(1);
	await expect(page.getByTestId('now-playing')).toContainText('Continuity A');

	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate(() => {
		localStorage.setItem('nntune.library.auth-transition.v1', 'pending-play-change');
		localStorage.removeItem('nntune.library.auth-transition.v1');
	});
	await otherTab.close();
	await expect(page.getByRole('alert')).toContainText('Your session changed');
	await expect(page.getByTestId('audio-player')).toHaveCount(0);

	await page.evaluate(() => {
		const w = window as unknown as { __holdPlay: boolean; __reject: ((e: unknown) => void)[] };
		w.__holdPlay = false;
		for (const reject of w.__reject.splice(0)) reject(new DOMException('blocked', 'NotAllowedError'));
	});
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(2);
	await openQueue(page);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
	await expect(page.getByTestId('queue-announcement')).toHaveText('');
	await expect(page.locator('main')).not.toContainText(/Selected Continuity|Playing Continuity/);
	expect(errors).toEqual([]);
});
