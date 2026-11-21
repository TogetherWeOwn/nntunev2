import { expect, test, type Page } from '@playwright/test';

type Probe = {
	handlers: Record<string, (() => void) | null>;
	retired: (() => void)[];
	retiredPlays: number;
};
declare global {
	interface Window { __librarySystemTransport: Probe; }
}

const coral = { id: 'system-coral', title: 'System Coral', artist: 'Fixture Artist', streamUrl: '/api/demo/audio?track=coral' };

async function openWithSystemControls(page: Page) {
	await page.addInitScript(() => {
		const handlers: Probe['handlers'] = {};
		window.__librarySystemTransport = { handlers, retired: [], retiredPlays: 0 };
		Object.defineProperty(navigator, 'mediaSession', {
			value: {
				metadata: null,
				setActionHandler(action: string, handler: (() => void) | null) { handlers[action] = handler; }
			}, configurable: true
		});
	});
	await page.route('**/api/library', (route) => route.fulfill({ json: [coral] }));
	await page.goto(`/library?track=${coral.id}&t=2`);
	await expect(page.getByTestId('audio-player')).toBeVisible();
	await expect.poll(() => page.evaluate(() => navigator.mediaSession.metadata?.title)).toBe(coral.title);
	await page.evaluate(() => {
		const probe = window.__librarySystemTransport;
		probe.retired = [probe.handlers.play!, probe.handlers.pause!];
	});
}

const systemAction = (page: Page, action: 'play' | 'pause') =>
	page.evaluate((key) => window.__librarySystemTransport.handlers[key]!(), action);

test('system Play and Pause control the same native decoder after leaving Library', async ({ page }) => {
	await openWithSystemControls(page);
	const native = page.locator('audio[controls][data-testid="audio-player"]');
	const audio = (await native.elementHandle())!;
	const time = () => audio.evaluate((el: HTMLAudioElement) => el.currentTime);
	await expect.poll(time).toBeCloseTo(2);
	await page.getByTestId('shell-rail').getByRole('link', { name: 'Home', exact: true }).click();
	await expect(page).toHaveURL(/\/home$/);
	await expect(page.getByTestId('library-list')).toHaveCount(0);
	const player = page.getByRole('region', { name: 'Player', exact: true });
	await systemAction(page, 'play');
	await expect(player.getByRole('button', { name: 'Pause', exact: true })).toBeEnabled();
	await expect.poll(time).toBeGreaterThan(2.1);
	await systemAction(page, 'pause');
	await expect(player.getByRole('status')).toHaveText('Paused');
	expect(await audio.evaluate((el: HTMLAudioElement) => el === document.querySelector('audio') && el.paused)).toBe(true);
	await expect(page.locator('audio')).toHaveCount(1);

	await page.evaluate(() => window.dispatchEvent(new StorageEvent('storage', { key: 'nntune.library.auth-transition.v1' })));
	await expect(native).toHaveCount(0);
	expect(await page.evaluate(() => {
		const { play, pause } = window.__librarySystemTransport.handlers;
		return [navigator.mediaSession.metadata, play, pause];
	})).toEqual([null, null, null]);
});

for (const path of ['/pricing', '/demo/player']) {
	test(`navigation to ${path} retires the app's system controls`, async ({ page }) => {
		await openWithSystemControls(page);
		const audio = (await page.getByTestId('audio-player').elementHandle())!;
		await audio.evaluate((el: HTMLAudioElement) => {
			el.play = async () => { window.__librarySystemTransport.retiredPlays += 1; };
		});
		await page.evaluate((href) => {
			const link = Object.assign(document.createElement('a'), { href });
			document.body.append(link);
			link.click();
		}, path);
		await expect(page).toHaveURL(new RegExp(`${path}$`));
		await expect(page.getByTestId('audio-player')).toHaveCount(0);
		const snapshot = () => page.evaluate(() => ({
			title: navigator.mediaSession.metadata?.title ?? null,
			active: Object.keys(window.__librarySystemTransport.handlers).filter((action) =>
				window.__librarySystemTransport.handlers[action]).sort()
		}));
		if (path === '/demo/player') {
			await expect(page.getByTestId('demo-audio')).toBeVisible();
			await expect.poll(async () => (await snapshot()).active).toContain('seekto');
			expect((await snapshot()).title).toBeTruthy();
			expect((await snapshot()).title).not.toBe(coral.title);
		} else {
			await expect.poll(snapshot).toEqual({ title: null, active: [] });
		}
		const current = await snapshot();
		await page.evaluate(() => window.__librarySystemTransport.retired.forEach((action) => action()));
		expect(await snapshot()).toEqual(current);
		expect(await page.evaluate(() => window.__librarySystemTransport.retiredPlays)).toBe(0);
		if (path === '/demo/player') {
			expect(await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => el.paused)).toBe(true);
		}
	});
}
