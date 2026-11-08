import { expect, test } from '@playwright/test';

/**
 * Demo route integration for listen-event telemetry. Media play/pause are
 * doubled (play dispatches `playing`); the real page, action and sender run.
 * The library route is covered by listen-events-library.spec.ts.
 */
type Body = { playback_id: string; track_id: string; event: string };

test('demo player emits start on playing, complete on end, and survives a 401', async ({ page }) => {
	const bodies: Body[] = [];
	let status = 200;
	await page.route('**/v1/plays/events', async (route) => {
		bodies.push(route.request().postDataJSON() as Body);
		await route.fulfill({ status, contentType: 'application/json', body: '{}' });
	});
	await page.route('**/api/demo/audio**', (route) => route.fulfill({ status: 200, body: '' }));
	await page.addInitScript(() => {
		HTMLMediaElement.prototype.play = function () {
			queueMicrotask(() => this.dispatchEvent(new Event('playing')));
			return Promise.resolve();
		};
		HTMLMediaElement.prototype.pause = function () {};
	});
	await page.goto('/demo/player');
	await page.getByTestId('queue-select-demo-track').click();
	await expect.poll(() => bodies.length).toBeGreaterThanOrEqual(1);
	const first = bodies[0];
	expect(first.event).toBe('start');
	expect(first.playback_id).toMatch(/^[0-9a-f-]{36}$/);
	expect(Object.keys(first).sort()).toEqual(['event', 'playback_id', 'track_id']);

	await page.getByTestId('demo-audio').evaluate((el) => el.dispatchEvent(new Event('ended')));
	await expect
		.poll(() => bodies.some((b) => b.playback_id === first.playback_id && b.event === 'complete'))
		.toBe(true);
	// Playback still works after telemetry is refused.
	status = 401;
	await page.getByTestId('queue-select-second-signal').click();
	await expect.poll(() => bodies.some((b) => b.track_id === 'second-signal')).toBe(true);
	await expect(page.getByTestId('player-error')).toHaveCount(0);
});
