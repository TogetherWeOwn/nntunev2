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

test('a logout in another tab drops identity and stops the decoder while expanded', async ({ page }) => {
	await page.setViewportSize({ width: 390, height: 844 });
	const decoder = await startPlayback(page);
	await miniLink(page).click();
	await expectExpanded(page);

	const actor = await page.context().newPage();
	try {
		await actor.route('**/v1/session/logout', (route) => route.fulfill({ json: { ok: true } }));
		await actor.goto('/now-playing');
		await expect(actor.locator('html')).toHaveAttribute('data-hydrated', '');
		await actor.getByRole('button', { name: 'Account menu' }).click();
		await actor.getByRole('menuitem', { name: 'Sign out' }).click();
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
