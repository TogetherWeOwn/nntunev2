import { expect, test } from '@playwright/test';
import { contextPath, eventsPath, watchListenContext } from './fixtures/listen-context.js';

test('demo records actual playback and keeps playing when history is refused', async ({ page }) => {
	const context = 'a'.repeat(43);
	const bodies: { playback_id: string; track_id: string; event: string; context: string }[] = [];
	let status = 200;
	await watchListenContext(page);
	await page.route(contextPath, (route) => route.fulfill({ json: { context } }));
	await page.route(eventsPath, (route) => {
		bodies.push(route.request().postDataJSON());
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
	await page.getByTestId('queue-select-demo-track').click();
	await expect.poll(() => bodies[0]).toEqual({
		playback_id: expect.stringMatching(/^[0-9a-f-]{36}$/), track_id: 'demo-track', event: 'start', context
	});
	await page.getByTestId('demo-audio').evaluate((el) => el.dispatchEvent(new Event('ended')));
	await expect.poll(() => bodies).toContainEqual({ ...bodies[0], event: 'complete' });
	await expect.poll(() => bodies.some((b) => b.track_id === 'second-signal' && b.event === 'start')).toBe(true);
	status = 401;
	const refused = page.waitForResponse((res) => res.url().endsWith('/v1/plays/events') && res.status() === 401 &&
		res.request().postDataJSON()?.track_id === 'third-light');
	await page.getByTestId('queue-select-third-light').click();
	await refused;
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await expect(page.getByTestId('player-error')).toHaveCount(0);
});
