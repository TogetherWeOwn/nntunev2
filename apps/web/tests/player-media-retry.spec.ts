import { expect, test, type Page } from '@playwright/test';

const coral = { id: 'retry-coral', title: 'Coral', streamUrl: '/api/demo/audio?track=coral' };
const moss = { id: 'retry-moss', title: 'Moss', streamUrl: '/api/demo/audio?track=moss' };
const coralMedia = `**${coral.streamUrl}`;

async function openPlayer(page: Page) {
	await page.route('**/api/library', (route) => route.fulfill({ json: [coral, moss] }));
	await page.goto(`/library?track=${coral.id}&t=2`);
	const player = page.getByRole('region', { name: 'Player', exact: true });
	const audio = player.getByTestId('audio-player');
	await expect(audio).toBeVisible();
	return {
		audio, status: player.getByRole('status'),
		retry: player.getByRole('button', { name: 'Retry playback', exact: true }),
		play: player.getByRole('button', { name: 'Play', exact: true }),
		pause: player.getByRole('button', { name: 'Pause', exact: true })
	};
}

const failInitialLoad = (page: Page) => page.route(coralMedia, (route) =>
	route.fulfill({ status: 404, body: '' }), { times: 1 });

test('keyboard Retry resumes real audio on the same decoder without automatic or duplicate reloads', async ({ page }) => {
	let retryRequests = 0;
	const requested = Promise.withResolvers<void>();
	const release = Promise.withResolvers<void>();
	await page.route(coralMedia, async (route) => {
		retryRequests += 1;
		requested.resolve();
		await release.promise;
		await route.continue();
	}, { times: 1 });
	await failInitialLoad(page);
	const { audio, status, retry, pause } = await openPlayer(page);
	try {
		await expect(retry).toBeEnabled();
		const node = (await audio.elementHandle())!;
		await node.evaluate((element: HTMLAudioElement) => {
			const load = element.load.bind(element);
			element.load = () => {
				element.dataset.loads = String(Number(element.dataset.loads ?? '0') + 1);
				load();
			};
		});
		expect(await node.evaluate((el: HTMLAudioElement) => !!el.error)).toBe(true);
		expect(retryRequests).toBe(0);
		await retry.press('Enter');
		await requested.promise;
		await expect(status).toHaveText('Retrying playback…');
		await expect(retry).toBeDisabled();
		await expect(retry).toBeFocused();
		await page.keyboard.press('Enter');
		await expect(audio).toHaveAttribute('data-loads', '1');
		release.resolve();
		await expect(status).toHaveText('Playing');
		await expect(pause).toBeFocused();
		await expect.poll(() => node.evaluate((el: HTMLAudioElement) => el.currentTime)).toBeGreaterThan(0.2);
		expect(await node.evaluate((el: HTMLAudioElement) =>
			el === document.querySelector('audio') && el.controls && !el.error && el.seekable.length > 0)).toBe(true);
		await expect(audio).toHaveAttribute('src', coral.streamUrl);
		await expect(audio).toHaveAttribute('data-loads', '1');
		await expect(page.locator('audio')).toHaveCount(1);
		await expect(page.getByTestId('now-playing')).toContainText('Coral');
		await expect(page.getByTestId('player-next')).toBeVisible();
	} finally {
		release.resolve();
	}
});

test('refused Play on healthy media stays Play without reloading or losing position', async ({ page }) => {
	const { audio, status, retry, play } = await openPlayer(page);
	const node = (await audio.elementHandle())!;
	const time = () => node.evaluate((el: HTMLAudioElement) => el.currentTime);
	await expect.poll(time).toBeCloseTo(2);
	await node.evaluate((el: HTMLAudioElement) => {
		const play = el.play.bind(el);
		el.play = () => {
			el.play = play;
			return Promise.reject(new DOMException('Fixture refusal', 'NotAllowedError'));
		};
		el.addEventListener('loadstart', () => { el.dataset.reloaded = 'yes'; });
	});
	await play.press('Enter');
	await expect(status).toContainText('Playback was blocked.');
	await expect(play).toBeEnabled();
	await expect(retry).toHaveCount(0);
	expect(await time()).toBeCloseTo(2);
	await play.press('Enter');
	await expect(status).toHaveText('Playing');
	await expect.poll(time).toBeGreaterThan(2.1);
	expect(await node.evaluate((el: HTMLAudioElement) => el.isConnected && !el.dataset.reloaded && !el.error)).toBe(true);
});

for (const boundary of ['new selection', 'auth transition'] as const) {
	test(`in-flight Retry retires with its decoder on ${boundary}`, async ({ page }) => {
		const requested = Promise.withResolvers<void>();
		const release = Promise.withResolvers<void>();
		const responded = Promise.withResolvers<void>();
		await page.route(coralMedia, async (route) => {
			requested.resolve();
			await release.promise;
			// The old decoder is removed before the blocked response is released.
			await route.fulfill({ status: 200, contentType: 'audio/wav', body: 'stale response' }).catch(() => {});
			responded.resolve();
		}, { times: 1 });
		await failInitialLoad(page);
		const { audio, status, retry } = await openPlayer(page);
		try {
			await expect(retry).toBeEnabled();
			const old = (await audio.elementHandle())!;
			await old.evaluate((el: HTMLAudioElement) => {
				const play = el.play.bind(el);
				el.play = () => play().finally(() => { el.dataset.settled = 'yes'; });
			});
			await retry.click();
			await requested.promise;
			await expect(status).toHaveText('Retrying playback…');
			if (boundary === 'new selection') {
				await page.getByRole('button', { name: 'More actions for Moss', exact: true }).click();
				await page.getByRole('menuitem', { name: 'Play now', exact: true }).click();
				await expect(audio).toHaveAttribute('src', moss.streamUrl);
			} else {
				// Deliver the same browser signal another tab publishes on sign-out.
				await page.evaluate(() => window.dispatchEvent(new StorageEvent('storage', {
					key: 'nntune.library.auth-transition.v1'
				})));
				await expect(audio).toHaveCount(0);
			}
			release.resolve();
			await responded.promise;
			await expect.poll(() => old.evaluate((el: HTMLAudioElement) =>
				!el.isConnected && el.paused && el.dataset.settled === 'yes')).toBe(true);
			await expect(retry).toHaveCount(0);
			if (boundary === 'new selection') {
				await expect(status).toHaveText('Playing');
				await expect(page.getByTestId('now-playing')).toContainText('Moss');
				await expect(page.locator('audio')).toHaveCount(1);
			} else {
				await expect(page.getByRole('alert')).toContainText(/session changed/i);
				await expect(page.getByRole('link', { name: 'Sign in', exact: true })).toHaveAttribute('href', '/login');
				await expect(page.getByTestId('now-playing')).toHaveCount(0);
				await expect(page.locator('audio')).toHaveCount(0);
			}
		} finally {
			release.resolve();
		}
	});
}
