import { expect, test, type Page } from '@playwright/test';
import { contextPath, eventsPath, watchListenContext } from '../fixtures/listen-context.js';

const context = 'a'.repeat(43);
type Body = { playback_id: string; track_id: string; event: string; context: string };

async function mountDemo(page: Page, { refuseThirdTrack = false, realDelivery = false } = {}) {
	let contextLoads = 0;
	const bodies: Body[] = [];
	await watchListenContext(page);
	// A second read would hand out a different scope, so reopening it cannot go unnoticed.
	await page.route(contextPath, (route) =>
		route.fulfill({ json: { context: ++contextLoads === 1 ? context : 'c'.repeat(43) } })
	);
	if (!realDelivery) await page.route(eventsPath, (route) => {
		const body = route.request().postDataJSON() as Body;
		bodies.push(body);
		const status = refuseThirdTrack && body.track_id === 'third-light' && body.event === 'start' ? 401 : 200;
		return route.fulfill({ status, json: {} });
	});
	// Double the media events and keep the media request pending, not empty.
	await page.route('**/api/demo/audio**', () => {});
	await page.addInitScript(() => {
		HTMLMediaElement.prototype.play = function () {
			queueMicrotask(() => this.dispatchEvent(new Event('playing')));
			return Promise.resolve();
		};
		HTMLMediaElement.prototype.pause = function () {};
	});
	await page.goto('/demo/player');
	await expect(page.locator('html')).toHaveAttribute('data-history-context-read', 'true');
	return { bodies, contextLoads: () => contextLoads };
}

function startResponse(page: Page) {
	return page.waitForResponse((response) => response.url().endsWith('/v1/plays/events') &&
		response.ok() && response.request().postDataJSON()?.event === 'start');
}

test('demo records actual playback and keeps playing when history is refused', async ({ page }) => {
	const { bodies } = await mountDemo(page, { refuseThirdTrack: true });
	await page.getByTestId('queue-select-demo-track').click();
	await expect.poll(() => bodies[0]).toEqual({
		playback_id: expect.stringMatching(/^[0-9a-f-]{36}$/), track_id: 'demo-track', event: 'start', context
	});
	await page.getByTestId('demo-audio').evaluate((el) => el.dispatchEvent(new Event('ended')));
	await expect.poll(() => bodies).toContainEqual({ ...bodies[0], event: 'complete' });
	await expect.poll(() => bodies.some((b) => b.track_id === 'second-signal' && b.event === 'start')).toBe(true);
	const refused = page.waitForResponse((res) => res.url().endsWith('/v1/plays/events') && res.status() === 401 &&
		res.request().postDataJSON()?.track_id === 'third-light');
	await page.getByTestId('queue-select-third-light').click();
	await refused;
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await expect(page.getByTestId('player-error')).toHaveCount(0);
});

test('reloading the document finishes the started listen', async ({ page, request }) => {
	await mountDemo(page, { realDelivery: true });
	const started = startResponse(page);
	await page.getByTestId('queue-select-demo-track').click();
	const body: Body = (await started).request().postDataJSON();
	await page.reload({ waitUntil: 'domcontentloaded' });
	await expect.poll(async () => {
		return (await request.get('/v1/plays/events', { params: { playback_id: body.playback_id } })).json();
	}).toEqual([body, { ...body, event: 'skip' }]);
});

test('a restored page starts a fresh listen without reopening the auth scope', async ({ page }) => {
	const { bodies, contextLoads } = await mountDemo(page);
	const pin = (id: string) => page.evaluate((id) => {
		(window as { __nntuneFixtureGrant?: string }).__nntuneFixtureGrant = id;
	}, id);
	await pin('A'.repeat(22));
	const started = startResponse(page);
	await page.getByTestId('queue-select-demo-track').click();
	await started;
	const persistedEvent = (type: 'pagehide' | 'pageshow') =>
		page.evaluate((type) => window.dispatchEvent(new PageTransitionEvent(type, { persisted: true })), type);
	const playing = () => page.getByTestId('demo-audio').evaluate((el) => el.dispatchEvent(new Event('playing')));
	await persistedEvent('pagehide');
	await expect.poll(() => bodies).toEqual([bodies[0], { ...bodies[0], event: 'skip' }]);
	await playing(); // Ignore queued media events while the page is hidden.
	await persistedEvent('pageshow');
	await playing(); // The same grant resumes the finished listen, so no new start.
	await pin('A'.repeat(21) + 'Q'); // An explicit new grant restarts with the original context.
	const restored = startResponse(page);
	await playing();
	await restored;
	expect(contextLoads()).toBe(1);
	expect(bodies).toEqual([
		bodies[0], { ...bodies[0], event: 'skip' },
		{ ...bodies[0], playback_id: '00000000-0000-0000-0000-000000000001' }
	]);
	await page.evaluate(() => window.dispatchEvent(new StorageEvent('storage', {
		key: 'nntune.library.auth-transition.v1'
	})));
	await persistedEvent('pagehide');
	await persistedEvent('pageshow');
	await page.getByTestId('demo-audio').evaluate((el) => el.dispatchEvent(new Event('playing')));
	await page.reload({ waitUntil: 'domcontentloaded' });
	expect(bodies.map((body) => body.event)).toEqual(['start', 'skip', 'start']);
});

test('pause keeps the listen but Next at the queue end abandons it', async ({ page }) => {
	const { bodies } = await mountDemo(page);
	const started = startResponse(page);
	await page.getByTestId('queue-select-third-light').click();
	await started;
	const toggle = () => page.keyboard.press('k');
	const status = page.getByTestId('player-status');
	await toggle();
	await expect(status).toHaveText('Status: paused');
	await toggle();
	await expect(status).toHaveText('Status: playing');
	await page.getByTestId('player-next').click();
	await expect(status).toHaveText('Status: ended');
	await expect.poll(() => bodies).toEqual([bodies[0], { ...bodies[0], event: 'skip' }]);
	await toggle();
	await expect.poll(() => bodies.map((body) => body.event)).toEqual(['start', 'skip', 'start']);
	expect(bodies[2].track_id).toBe(bodies[0].track_id);
	expect(bodies[2].playback_id).not.toBe(bodies[0].playback_id);
});
