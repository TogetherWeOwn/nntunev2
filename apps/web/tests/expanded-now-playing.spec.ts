import { expect, test, type Page } from '@playwright/test';

const track = {
	id: 'expanded-coral',
	title: 'Expanded Coral',
	artist: 'Fixture Artist',
	streamUrl: '/api/demo/audio?track=coral'
};
const nextTrack = { id: 'expanded-moss', title: 'Expanded Moss' };

const audio = (page: Page) => page.locator('audio[data-testid="audio-player"]');
const miniLink = (page: Page) => page.getByTestId('mini-player').getByRole('link');
const desktopLink = (page: Page) => page.getByTestId('shell-rail').getByRole('link', { name: 'Now Playing', exact: true });
const tab = (page: Page, name: string) =>
	page.getByTestId('shell-tabbar').getByRole('link', { name, exact: true });
const heading = (page: Page) =>
	page.getByRole('main').getByRole('heading', { name: 'Now Playing', exact: true });

async function serveLibrary(page: Page) {
	await page.route('**/api/library', (route) => route.fulfill({ json: [track, nextTrack] }));
}

async function startPlayback(page: Page) {
	await serveLibrary(page);
	await page.goto('/library');
	await page.getByRole('button', { name: 'Play displayed from the top' }).click();
	await expect(page.getByTestId('now-playing')).toContainText(track.title);
	await expect(page.getByTestId('player-next')).toBeVisible();
	const decoder = await page.waitForSelector('audio');
	// The existing same-origin fixture serves real Range-capable PCM WAV. No
	// replacement media methods or synthetic playback events are needed.
	await expect.poll(() => decoder.evaluate((node) =>
		!node.paused && node.seekable.length > 0 && node.currentTime > 0.2)).toBe(true);
	return decoder;
}

async function expectExpanded(page: Page) {
	await expect(page).toHaveURL(/\/now-playing$/);
	await expect(heading(page)).toBeFocused();
	await expect(page.getByRole('main').getByRole('heading', { name: track.title })).toBeVisible();
	await expect(page.getByRole('main').getByText(track.artist, { exact: true })).toBeVisible();
	await expect(page.getByRole('main').getByTestId('track-artwork')).toHaveAttribute('data-fallback', 'true');
	await expect(page.getByTestId('mini-player')).toHaveCount(0);
}

for (const width of [320, 360, 390]) {
	for (const paused of [false, true]) {
		test.describe(`${width}px ${paused ? 'paused' : 'playing'}`, () => {
			test.use({ viewport: { width, height: 844 }, hasTouch: true });

			test('mini entry, Back and Library navigation keep the same native decoder', async ({ page }) => {
				const decoder = await startPlayback(page);
				const before = await decoder.evaluate((node, paused) => {
					if (paused) node.pause();
					for (const event of ['loadstart', 'play']) {
						node.addEventListener(event, () => { node.dataset.restarted = event; });
					}
					return { position: node.currentTime, source: node.currentSrc };
				}, paused);
				let position = before.position;
				const time = () => decoder.evaluate((node) => node.currentTime);
				const expectSamePlayback = async () => {
					await expect(audio(page)).toBeVisible();
					await expect(audio(page)).toHaveAttribute('controls', '');
					await expect(page.getByTestId('library-now-playing')).toHaveText(`${track.title} — ${track.artist}`);
					await expect.poll(() => decoder.evaluate((node) => ({
						count: document.querySelectorAll('audio').length,
						sameNode: document.querySelector('audio') === node,
						source: node.currentSrc,
						paused: node.paused,
						restarted: node.dataset.restarted
					}))).toEqual({ count: 1, sameNode: true, source: before.source, paused, restarted: undefined });
					if (paused) expect(await time()).toBeCloseTo(position, 3);
					else await expect.poll(time).toBeGreaterThan(position + 0.1);
					position = await time();
				};

				// Open from Home to distinguish history Back from a hardcoded Library return.
				await tab(page, 'Home').click();
				await expect(page).toHaveURL(/\/home$/);
				await expectSamePlayback();
				const mini = miniLink(page);
				await expect(mini).toContainText(track.title);
				if (paused) await mini.press('Enter');
				else await mini.tap();
				await expectExpanded(page);
				await expectSamePlayback();

				if (paused) await page.goBack();
				else await page.getByRole('main').getByRole('button', { name: 'Back', exact: true }).click();
				await expect(page).toHaveURL(/\/home$/);
				await expect(mini).toBeFocused();
				await expectSamePlayback();

				await page.goForward();
				await expectExpanded(page);
				await expectSamePlayback();
				if (paused) {
					// The legacy native-player fallback is still wired on the expanded route.
					await page.getByTestId('library-restart').click();
					await expect.poll(time).toBeCloseTo(0, 3);
					position = 0;
				}
				await tab(page, 'Your Library').click();
				await expect(page).toHaveURL(/\/library$/);
				await expect(page.getByTestId('now-playing')).toContainText(track.title);
				await expect(page.getByTestId('player-next')).toBeVisible();
				await expectSamePlayback();
			});
		});
	}
}

test('direct entry is empty and offers standard Library access', async ({ page }) => {
	await serveLibrary(page);
	await page.goto('/now-playing');
	await expect(page.getByRole('main')).toHaveCount(1);
	await expect(heading(page)).toBeVisible();
	await expect(page).toHaveTitle('Now Playing — NNTune');
	await expect(page.getByText('Nothing playing.', { exact: false })).toBeVisible();
	await expect(page.locator('audio')).toHaveCount(0);
	await page.getByRole('main').getByRole('link', { name: 'Your Library' }).click();
	await expect(page).toHaveURL(/\/library$/);
});

async function prepareLogout(actor: Page) {
	await actor.route('**/v1/session/logout', (route) => route.fulfill({ json: { ok: true } }));
	await actor.goto('/now-playing');
	await expect(actor.locator('html')).toHaveAttribute('data-hydrated', '');
	await actor.getByRole('button', { name: 'Account menu' }).click();
	return actor.getByRole('menuitem', { name: 'Sign out' });
}

test('a logout in another tab drops identity and stops the decoder while expanded', async ({ page }) => {
	await page.setViewportSize({ width: 390, height: 844 });
	const decoder = await startPlayback(page);
	await miniLink(page).click();
	await expectExpanded(page);
	await expect(enterFullscreen(page)).toBeHidden();

	const actor = await page.context().newPage();
	try {
		const signOut = await prepareLogout(actor);
		await signOut.click();
		await expect(actor).toHaveURL(/\/login$/);
	} finally {
		await actor.close();
	}

	await expect(page).toHaveURL(/\/now-playing$/);
	await expect(page.getByText('Nothing playing.', { exact: false })).toBeVisible();
	await expect(page.getByText(track.title, { exact: false })).toHaveCount(0);
	await expect(page.getByText(track.artist, { exact: false })).toHaveCount(0);
	await expect(page.getByTestId('mini-player')).toHaveCount(0);
	await expect(page.locator('audio')).toHaveCount(0);
	expect(await decoder.evaluate((node) => node.paused && !node.isConnected)).toBe(true);
});

const enterFullscreen = (page: Page) => page.getByRole('button', { name: 'Full screen', exact: true });
const exitFullscreen = (page: Page) => page.getByRole('button', { name: 'Exit full screen', exact: true });
const back = (page: Page) => page.getByRole('main').getByRole('button', { name: 'Back', exact: true });
const noFullscreen = (page: Page) => page.evaluate(() => document.fullscreenElement === null);

async function openDesktopExpanded(page: Page) {
	await page.setViewportSize({ width: 1280, height: 800 });
	const decoder = await startPlayback(page);
	await expect(enterFullscreen(page)).toBeHidden();
	await desktopLink(page).press('Enter');
	await expectExpanded(page);
	await expect(enterFullscreen(page)).toBeVisible();
	return decoder;
}

test('fullscreen exit focuses the heading after logout clears the track', async ({ page }) => {
	const actor = await page.context().newPage();
	const signOut = await prepareLogout(actor);
	await page.bringToFront();
	await openDesktopExpanded(page);
	await enterFullscreen(page).click();
	await expect(exitFullscreen(page)).toBeFocused();

	// Exercise the real logout handler without changing the fullscreen tab.
	await signOut.evaluate((button: HTMLButtonElement) => button.click());
	await expect(actor).toHaveURL(/\/login$/);
	await expect(page.getByText('Nothing playing.', { exact: false })).toBeVisible();
	await expect(page.locator('audio')).toHaveCount(0);
	await expect(exitFullscreen(page)).toBeVisible();
	await page.evaluate(() => document.exitFullscreen());
	await expect(exitFullscreen(page)).toHaveCount(0);
	await expect(enterFullscreen(page)).toHaveCount(0);
	await expect(heading(page)).toBeFocused();
	await expect(page).toHaveURL(/\/now-playing$/);
});

for (const paused of [true, false]) {
	test(`native fullscreen preserves the ${paused ? 'paused' : 'playing'} decoder across entry, exit and Back`, async ({ page }) => {
		const decoder = await openDesktopExpanded(page);
		if (paused) {
			await decoder.evaluate((node) => new Promise<void>((resolve) => {
				node.addEventListener('pause', () => resolve(), { once: true });
				node.pause();
			}));
		}
		const before = await decoder.evaluate((node) => {
			for (const event of ['loadstart', 'play', 'pause', 'seeking']) {
				node.addEventListener(event, () => { node.dataset.fullscreenMediaEvent = event; });
			}
			// Record even no-op commands while keeping the real WAV playback and clock.
			const media = HTMLMediaElement.prototype;
			for (const command of ['play', 'pause', 'load', 'fastSeek'] as const) {
				const original = media[command];
				if (typeof original !== 'function') continue;
				Object.defineProperty(media, command, {
					value: function (this: HTMLMediaElement, ...args: unknown[]) {
						node.dataset.fullscreenMediaCommand = command;
						return Reflect.apply(original, this, args);
					}
				});
			}
			const clock = Object.getOwnPropertyDescriptor(media, 'currentTime')!;
			Object.defineProperty(media, 'currentTime', {
				...clock,
				set(this: HTMLMediaElement, value: number) {
					node.dataset.fullscreenMediaCommand = 'currentTime';
					clock.set!.call(this, value);
				}
			});
			return { source: node.currentSrc, position: node.currentTime };
		});
		let position = before.position;
		const time = () => decoder.evaluate((node) => node.currentTime);
		const expectSamePlayback = async () => {
			await expect(page.locator('audio')).toHaveCount(1);
			expect(await decoder.evaluate((node) => ({
				sameNode: document.querySelector('audio[data-testid="audio-player"]') === node,
				source: node.currentSrc,
				paused: node.paused,
				event: node.dataset.fullscreenMediaEvent,
				command: node.dataset.fullscreenMediaCommand,
				insideFullscreen: document.fullscreenElement?.contains(node) ?? false
			}))).toEqual({
				sameNode: true, source: before.source, paused, event: undefined,
				command: undefined, insideFullscreen: false
			});
			if (paused) expect(await time()).toBeCloseTo(position, 3);
			else await expect.poll(time).toBeGreaterThan(position + 0.1);
			position = await time();
		};

		const enter = enterFullscreen(page);
		const exit = exitFullscreen(page);
		await enter.press('Enter');
		await expect(exit).toBeFocused();
		await expect.poll(() => page.evaluate(() => {
			const surface = document.fullscreenElement;
			return surface?.matches('main#main-content') === true &&
				surface.contains(document.elementFromPoint(8, 8));
		})).toBe(true);
		await expectSamePlayback();

		await exit.click();
		await expect.poll(() => noFullscreen(page)).toBe(true);
		await expect(enter).toBeFocused();
		await expectSamePlayback();

		await enter.click();
		await expect(exit).toBeVisible();
		await back(page).focus();
		// Headless Chromium does not reliably route Escape through browser chrome.
		// A native exit outside the app exercises that same fullscreenchange path.
		await page.evaluate(() => document.exitFullscreen());
		await expect(enter).toBeFocused();
		await expectSamePlayback();

		await enter.click();
		await expect(exit).toBeVisible();
		await back(page).click();
		await expect(page).toHaveURL(/\/library$/);
		await expect.poll(() => noFullscreen(page)).toBe(true);
		await expect(enter).toHaveCount(0);
		await expectSamePlayback();
		await expect(page.getByTestId('now-playing')).toContainText(track.title);
		await expect(page.getByTestId('player-next')).toBeVisible();
	});
}

test('unavailable fullscreen leaves the expanded view and Back usable', async ({ page }) => {
	await page.addInitScript(() => {
		Object.defineProperty(document, 'fullscreenEnabled', { get: () => false });
	});
	await openDesktopExpanded(page);
	await expect(enterFullscreen(page)).toBeDisabled();
	await expect(page.getByText('Full screen is unavailable in this browser.')).toBeVisible();
	await expect.poll(() => noFullscreen(page)).toBe(true);
	await back(page).click();
	await expect(page).toHaveURL(/\/library$/);
	await expect(desktopLink(page)).toBeFocused();
});

test('refused fullscreen entry and exit are truthful and retryable', async ({ page }) => {
	await page.addInitScript(() => {
		const request = Element.prototype.requestFullscreen;
		const exit = Document.prototype.exitFullscreen;
		let refuseEntry = true;
		let refuseExit = true;
		Element.prototype.requestFullscreen = function (options) {
			if (!refuseEntry) return request.call(this, options);
			refuseEntry = false;
			return Promise.reject(new TypeError('Fixture refused entry'));
		};
		Document.prototype.exitFullscreen = function () {
			if (!refuseExit) return exit.call(this);
			refuseExit = false;
			return Promise.reject(new TypeError('Fixture refused exit'));
		};
	});
	await openDesktopExpanded(page);
	const enter = enterFullscreen(page);
	const exit = exitFullscreen(page);
	await enter.click();
	await expect(page.getByText('Could not enter full screen.')).toBeVisible();
	await expect.poll(() => noFullscreen(page)).toBe(true);
	await expect(enter).toBeFocused();

	await enter.click();
	await expect(exit).toBeFocused();
	await expect(page.getByText('Could not enter full screen.')).toHaveCount(0);
	await exit.click();
	await expect(page.getByText('Could not exit full screen.')).toBeVisible();
	await expect(exit).toBeFocused();
	await expect.poll(() => noFullscreen(page)).toBe(false);

	// The first app exit was refused. A subsequent browser-driven exit must
	// clear its status and restore focus even though the app button was not used.
	await back(page).focus();
	await page.evaluate(() => document.exitFullscreen());
	await expect(enter).toBeFocused();
	await expect.poll(() => noFullscreen(page)).toBe(true);
	await expect(page.getByText('Could not exit full screen.')).toHaveCount(0);
	await expect(page).toHaveURL(/\/now-playing$/);
});

test('leaving with entry pending retires the fullscreen request without affecting the next visit', async ({ page }) => {
	await page.addInitScript(() => {
		const request = Element.prototype.requestFullscreen;
		let hold = true;
		Element.prototype.requestFullscreen = function (options) {
			if (!hold) return request.call(this, options);
			this.setAttribute('data-held-fullscreen-requests', String(
				Number(this.getAttribute('data-held-fullscreen-requests')) + 1
			));
			return new Promise<void>((_, reject) => {
				document.addEventListener('release-fullscreen-fixture', () => {
					hold = false;
					reject(new TypeError('Fixture request retired'));
				}, { once: true });
			});
		};
	});
	await openDesktopExpanded(page);
	const enter = enterFullscreen(page);
	await enter.click();
	await expect(enter).toBeDisabled();
	await enter.evaluate((button) => (button as HTMLButtonElement).click());
	await expect(page.getByRole('main')).toHaveAttribute('data-held-fullscreen-requests', '1');
	await back(page).click();
	await expect(page).toHaveURL(/\/library$/);
	await page.goForward();
	await expectExpanded(page);
	await page.evaluate(() => document.dispatchEvent(new Event('release-fullscreen-fixture')));
	await expect(enter).toBeEnabled();
	await expect(heading(page)).toBeFocused();
	await expect(page.getByText('Could not enter full screen.')).toHaveCount(0);
	await enter.click();
	await expect(exitFullscreen(page)).toBeFocused();
});
