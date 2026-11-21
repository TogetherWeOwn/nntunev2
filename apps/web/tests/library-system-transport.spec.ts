import { expect, test, type Page } from '@playwright/test';

type Probe = {
	handlers: Record<string, (() => void) | null>;
	retired: Record<string, (() => void) | null>;
	retiredPlays: number;
};
declare global {
	interface Window { __librarySystemTransport: Probe; }
}

const coral = {
	id: 'system-coral', title: 'System Coral', artist: 'Fixture Artist', release: 'Fixture Album',
	streamUrl: '/api/demo/audio?track=coral'
};

async function openWithSystemControls(page: Page, items = [coral]) {
	await page.addInitScript(() => {
		const handlers: Probe['handlers'] = {};
		window.__librarySystemTransport = { handlers, retired: {}, retiredPlays: 0 };
		Object.defineProperty(navigator, 'mediaSession', {
			value: {
				metadata: null,
				setActionHandler(action: string, handler: (() => void) | null) { handlers[action] = handler; }
			}, configurable: true
		});
	});
	await page.route('**/api/library', (route) => route.fulfill({ json: items }));
	await page.goto(`/library?track=${coral.id}&t=2`);
	await expect(page.getByTestId('audio-player')).toBeVisible();
	await expect.poll(() => page.evaluate(() => {
		const metadata = navigator.mediaSession.metadata;
		return [metadata?.title, metadata?.artist, metadata?.album];
	})).toEqual([coral.title, coral.artist, coral.release]);
	await page.evaluate(() => {
		const probe = window.__librarySystemTransport;
		probe.retired = { ...probe.handlers };
	});
}

const systemAction = (page: Page, action: 'play' | 'pause' | 'nexttrack' | 'previoustrack') =>
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
	await expect(native).toBeVisible();
	expect(await audio.evaluate((el: HTMLAudioElement) => el === document.querySelector('audio') && el.controls && el.paused)).toBe(true);
	const pausedAt = await time();
	expect(pausedAt).toBeCloseTo(2);
	const playedAt = await audio.evaluate((el: HTMLAudioElement) => new Promise<number>((resolve) => {
		el.addEventListener('play', () => resolve(el.currentTime), { once: true });
		window.__librarySystemTransport.handlers.play!();
	}));
	expect(Math.abs(playedAt - pausedAt)).toBeLessThan(0.25);
	await expect(player.getByRole('status')).toHaveText('Playing');
	await expect.poll(time).toBeGreaterThan(pausedAt + 0.1);
	await systemAction(page, 'pause');
	await expect(player.getByRole('status')).toHaveText('Paused');
	expect(await audio.evaluate((el: HTMLAudioElement) => el === document.querySelector('audio[controls]') && el.paused)).toBe(true);
	await expect(page.locator('audio')).toHaveCount(1);

	// Exercise the native media path independently of both sets of app actions.
	await audio.evaluate((el: HTMLAudioElement) => el.play());
	await expect(player.getByRole('status')).toHaveText('Playing');
	await audio.evaluate((el: HTMLAudioElement) => el.pause());
	await expect(player.getByRole('status')).toHaveText('Paused');

	await page.evaluate(() => window.dispatchEvent(new StorageEvent('storage', { key: 'nntune.library.auth-transition.v1' })));
	await expect(native).toHaveCount(0);
	expect(await page.evaluate(() => {
		const { play, pause } = window.__librarySystemTransport.handlers;
		return [navigator.mediaSession.metadata, play, pause];
	})).toEqual([null, null, null]);
});

test('system Previous and Next navigate the persistent queue without autoplay', async ({ page }) => {
	const second = { ...coral, id: 'system-second', title: 'System Second', streamUrl: '/api/demo/audio?track=second-signal' };
	await openWithSystemControls(page, [coral, second]);
	await page.getByTestId('library-item').and(page.locator(`[data-id="${second.id}"]`)).click();
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	await page.getByTestId('shell-rail').getByRole('link', { name: 'Home', exact: true }).click();
	await expect(page).toHaveURL(/\/home$/);
	await expect(page.getByTestId('library-list')).toHaveCount(0);

	const native = page.getByTestId('audio-player');
	const firstAudio = (await native.elementHandle())!;
	await systemAction(page, 'previoustrack');
	expect(await firstAudio.evaluate((el) => el.isConnected && (el as HTMLAudioElement).paused)).toBe(true);

	await systemAction(page, 'nexttrack');
	await expect(native).toHaveAttribute('src', second.streamUrl);
	await expect(native).toHaveJSProperty('paused', true);
	const title = () => page.evaluate(() => navigator.mediaSession.metadata?.title);
	await expect.poll(title).toBe(second.title);
	const secondAudio = (await native.elementHandle())!;
	await page.evaluate(() => window.__librarySystemTransport.retired.previoustrack!());
	await expect(native).toHaveAttribute('src', second.streamUrl);

	await systemAction(page, 'play');
	const status = page.getByRole('region', { name: 'Player', exact: true }).getByRole('status');
	await expect(status).toHaveText('Playing');
	await systemAction(page, 'nexttrack');
	expect(await secondAudio.evaluate((el) => el.isConnected && !(el as HTMLAudioElement).paused)).toBe(true);

	await systemAction(page, 'previoustrack');
	await expect(native).toHaveAttribute('src', coral.streamUrl);
	await expect(status).toHaveText('Paused');
	await expect(native).toHaveJSProperty('paused', true);
	expect(await secondAudio.evaluate((el) => !el.isConnected && (el as HTMLAudioElement).paused)).toBe(true);
	await expect.poll(title).toBe(coral.title);
	await expect(page.locator('audio')).toHaveCount(1);

	const afterPurge = await page.evaluate(() => {
		const { handlers } = window.__librarySystemTransport;
		const { nexttrack, previoustrack } = handlers;
		window.dispatchEvent(new StorageEvent('storage', { key: 'nntune.library.auth-transition.v1' }));
		nexttrack!();
		previoustrack!();
		return [navigator.mediaSession.metadata, handlers.nexttrack, handlers.previoustrack];
	});
	expect(afterPurge).toEqual([null, null, null]);
	await expect(native).toHaveCount(0);
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
		const retiredAction = (action: 'play' | 'pause') =>
			page.evaluate((key) => window.__librarySystemTransport.retired[key]!(), action);
		if (path === '/demo/player') {
			const demo = page.getByTestId('demo-audio');
			await expect(demo).toBeVisible();
			await expect.poll(async () => (await snapshot()).active).toContain('seekto');
			const current = await snapshot();
			expect(current.title).toBeTruthy();
			expect(current.title).not.toBe(coral.title);
			await retiredAction('play');
			await expect(demo).toHaveJSProperty('paused', true);
			await systemAction(page, 'play');
			await expect(demo).toHaveJSProperty('paused', false);
			await retiredAction('pause');
			expect(await demo.evaluate((el: HTMLAudioElement) => el.paused)).toBe(false);
			await systemAction(page, 'pause');
			await expect(demo).toHaveJSProperty('paused', true);
			expect(await snapshot()).toEqual(current);
		} else {
			const cleared = { title: null, active: [] };
			await expect.poll(snapshot).toEqual(cleared);
			await retiredAction('play');
			expect(await snapshot()).toEqual(cleared);
			await retiredAction('pause');
			expect(await snapshot()).toEqual(cleared);
		}
		expect(await page.evaluate(() => window.__librarySystemTransport.retiredPlays)).toBe(0);
	});
}
