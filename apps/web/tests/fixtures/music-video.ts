import { expect, type Locator, type Page } from '@playwright/test';

export const title = 'Fixture Video Track';
const linked = {
	id: 'cfa3bd0c-52a8-4b00-8f28-993d00001601',
	title,
	artist: 'Fixture Artist',
	streamUrl: '/api/demo/audio?track=coral'
};
export const youtubeOrigin = 'https://www.youtube.com';
export const embedOrigin = 'https://www.youtube-nocookie.com';
const thirdParty = /^https:\/\/[^/]*(?:youtube(?:-nocookie)?\.com|ytimg\.com|doubleclick\.net|googleapis\.com|policies\.google\.com)\//;

export async function openNowPlaying(page: Page, enabled = false) {
	const requests: string[] = [];
	await page.route(thirdParty, (route) => {
		const url = new URL(route.request().url());
		requests.push(url.origin + url.pathname);
		if (url.href === `${youtubeOrigin}/iframe_api`) {
			return route.fulfill({ contentType: 'text/javascript', body: `
				let first = true;
				window.YT = { Player: class {
					constructor(frame, { events }) {
						frame.addEventListener('load', () => {
							if (first) {
								first = false;
								events.onError();
							} else events.onReady({ target: this });
						}, { once: true });
					}
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
	await page.addInitScript(() => {
		const setHandler = navigator.mediaSession.setActionHandler.bind(navigator.mediaSession);
		navigator.mediaSession.setActionHandler = (action, handler) => {
			setHandler(action, handler);
			if (action === 'nexttrack') {
				Object.assign(window, { fixtureNextTrack: () => handler?.({ action }) });
			}
		};
	});
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

export async function playerFootprint(frame: Locator) {
	return frame.evaluate((element) => {
		const { left, top, width, height } = element.getBoundingClientRect();
		const unobscured = [1, width - 1].every((x) =>
			[1, height - 1].every((y) => document.elementFromPoint(left + x, top + y) === element));
		return { width, height, unobscured };
	});
}

export async function setDocumentHidden(page: Page, hidden: boolean) {
	await page.evaluate((hidden) => {
		if (hidden) Object.defineProperty(document, 'hidden', { value: true, configurable: true });
		else Reflect.deleteProperty(document, 'hidden');
		document.dispatchEvent(new Event('visibilitychange'));
	}, hidden);
}
