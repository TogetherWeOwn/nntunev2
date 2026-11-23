import { expect, test, type Page } from '@playwright/test';

const title = 'Fixture Video Track';
const linked = {
	id: 'cfa3bd0c-52a8-4b00-8f28-993d00001601',
	title,
	artist: 'Fixture Artist',
	streamUrl: '/api/demo/audio?track=coral'
};
const youtubeOrigin = 'https://www.youtube.com';
const embedOrigin = 'https://www.youtube-nocookie.com';
const thirdParty = /^https:\/\/[^/]*(?:youtube(?:-nocookie)?\.com|ytimg\.com|doubleclick\.net|googleapis\.com|policies\.google\.com)\//;

async function openNowPlaying(page: Page, enabled = false) {
	const requests: string[] = [];
	await page.route(thirdParty, (route) => {
		const url = new URL(route.request().url());
		requests.push(url.origin + url.pathname);
		if (url.href === `${youtubeOrigin}/iframe_api`) {
			return route.fulfill({ contentType: 'text/javascript', body: `
				window.YT = { Player: class {
					constructor(frame, { events }) { queueMicrotask(() => events.onReady({ target: this })); }
					playVideo() { document.body.dataset.videoState = 'playing'; }
					pauseVideo() { document.body.dataset.videoState = 'paused'; }
					destroy() {}
				} };
				window.onYouTubeIframeAPIReady();
			` });
		}
		if (url.origin === embedOrigin && url.pathname === '/embed/fixture0001') {
			return route.fulfill({ contentType: 'text/html', body: '<button>Fixture native play control</button>' });
		}
		return route.abort();
	});
	await page.route('**/api/library', (route) => route.fulfill({
		json: [linked, { id: 'video-unlinked', title: 'Unlinked Track', artist: linked.artist, streamUrl: linked.streamUrl }]
	}));
	await page.goto('/library');
	await page.getByRole('button', { name: 'Play displayed from the top' }).click();
	const decoder = await page.waitForSelector('audio[data-testid="audio-player"]');
	await expect.poll(() => decoder.evaluate((node) => !node.paused && node.currentTime > 0.2)).toBe(true);
	await page.evaluate((fixture) => {
		// Link navigation keeps the real layout and its decoder mounted.
		const link = document.createElement('a');
		link.href = fixture ? '/now-playing?music-video-fixture=on' : '/now-playing';
		document.body.append(link);
		link.click();
		link.remove();
	}, enabled);
	await expect(page.getByRole('heading', { name: 'Now Playing', exact: true })).toBeVisible();
	return { decoder, requests };
}

test('the default omits video entry points and does not contact YouTube for a linked recording', async ({ page }) => {
	const { decoder, requests } = await openNowPlaying(page);
	await expect(page.getByRole('heading', { name: title })).toBeVisible();
	await expect(page.getByRole('tab', { name: 'Music Video' })).toHaveCount(0);
	await expect(page.getByText('Watch video')).toHaveCount(0);
	await expect(page.locator('iframe, script[src*="youtube"], link[href*="youtube"], link[href*="ytimg"]')).toHaveCount(0);
	expect(requests).toEqual([]);
	expect(await decoder.evaluate((node) => node.isConnected && !node.paused)).toBe(true);
});

test('verified video stays visible, retires on hide and preserves the audio queue', async ({ page }) => {
	await page.setViewportSize({ width: 320, height: 700 });
	await page.addInitScript(() => {
		const setHandler = navigator.mediaSession.setActionHandler.bind(navigator.mediaSession);
		navigator.mediaSession.setActionHandler = (action, handler) => {
			setHandler(action, handler);
			if (action === 'nexttrack') {
				Object.assign(window, { fixtureNextTrack: () => handler?.({ action }) });
			}
		};
	});
	const { decoder, requests } = await openNowPlaying(page, true);
	const audioTab = page.getByRole('tab', { name: 'Audio' });
	const videoTab = page.getByRole('tab', { name: 'Music Video' });
	const watch = page.getByRole('button', { name: 'Watch video' });
	await audioTab.press('ArrowRight');
	await expect(videoTab).toBeFocused();
	await expect(audioTab).toHaveAttribute('aria-selected', 'true');
	await videoTab.press('Enter');
	await expect(watch).toBeVisible();
	await expect(page.getByRole('link', { name: 'YouTube Terms of Service' })).toBeVisible();
	await expect(page.getByRole('link', { name: 'Google Privacy Policy' })).toBeVisible();
	expect(requests).toEqual([]);

	await watch.click();
	const frame = page.locator('iframe[title="Fixture Video Track on YouTube"]');
	await expect(page.frameLocator('iframe').getByRole('button', { name: 'Fixture native play control' })).toBeVisible();
	await expect(page.locator('body')).toHaveAttribute('data-video-state', 'playing');
	await expect(frame).toHaveAttribute('referrerpolicy', 'strict-origin-when-cross-origin');
	const embed = new URL((await frame.getAttribute('src'))!);
	expect(embed.searchParams.get('origin')).toBe(new URL(page.url()).origin);
	expect(requests).toEqual([`${youtubeOrigin}/iframe_api`, `${embedOrigin}/embed/fixture0001`]);
	expect(await frame.evaluate((element) => {
		const { left, top, width, height } = element.getBoundingClientRect();
		return width >= 200 && height >= 200 && element.closest('dialog:modal') !== null &&
			[1, width - 1].every((x) =>
				[1, height - 1].every((y) => document.elementFromPoint(left + x, top + y) === element)
			);
	})).toBe(true);
	const position = await decoder.evaluate((node) => node.currentTime);
	expect(await decoder.evaluate((node) => node.isConnected && node.paused)).toBe(true);

	await page.evaluate(() => {
		Object.defineProperty(document, 'hidden', { value: true, configurable: true });
		document.dispatchEvent(new Event('visibilitychange'));
	});
	await expect(page.locator('body')).toHaveAttribute('data-video-state', 'paused');
	await expect(frame).toHaveCount(0);
	await page.evaluate(() => {
		Reflect.deleteProperty(document, 'hidden');
		document.dispatchEvent(new Event('visibilitychange'));
	});
	await expect(frame).toHaveCount(0);
	expect(await decoder.evaluate((node) => ({ position: node.currentTime, paused: node.paused })))
		.toEqual({ position, paused: true });

	await watch.click();
	await expect(frame).toBeVisible();
	await page.setViewportSize({ width: 190, height: 700 });
	await expect(page.locator('body')).toHaveAttribute('data-video-state', 'paused');
	await expect(frame).toHaveCount(0);
	const before = [...requests];
	await watch.click();
	await expect(page.getByText('The video player needs more room.')).toBeVisible();
	expect(requests).toEqual(before);
	await page.setViewportSize({ width: 320, height: 700 });
	await watch.click();
	await expect(page.locator('body')).toHaveAttribute('data-video-state', 'playing');
	await page.evaluate(() => (window as Window & { fixtureNextTrack: () => void }).fixtureNextTrack());
	await expect(page.getByRole('heading', { name: 'Unlinked Track' })).toBeVisible();
	await expect(videoTab).toHaveCount(0);
	await expect(frame).toHaveCount(0);
	await expect(page.locator('body')).toHaveAttribute('data-video-state', 'paused');
});
