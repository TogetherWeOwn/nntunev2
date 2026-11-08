import { expect, test, type Page } from '@playwright/test';

/**
 * Library route integration for listen-event telemetry. Fixtures follow
 * library-repeat-end-policy.spec.ts: route-mocked `/api/library`, pending
 * `/test-audio/**`, doubled media play/pause, synthetic media events. The real
 * page, action and sender run; `/v1/plays/events` is fulfilled in-test.
 */
type Body = { playback_id: string; track_id: string; event: string };

const ITEMS = [
	{ id: 'lib-coral', title: 'Coral Night', artist: 'Mira', streamUrl: '/test-audio/lib-coral' },
	{ id: 'lib-pine', title: 'Pine Morning', artist: 'North', streamUrl: '/test-audio/lib-pine' }
];

function row(page: Page, id: string) {
	return page.locator(`[data-testid="library-item"][data-id="${id}"]`);
}

async function selectRow(page: Page, id: string, title: string) {
	await row(page, id).getByRole('button', { name: new RegExp(`^${title}( — |$)`) }).click();
}

async function playing(page: Page) {
	await page.getByTestId('audio-player').evaluate((el) => {
		void (el as HTMLMediaElement).play();
		el.dispatchEvent(new Event('play'));
		el.dispatchEvent(new Event('playing'));
	});
}

test('library emits start, skip for the replaced source, then complete', async ({ page }) => {
	const bodies: Body[] = [];
	await page.route('**/v1/plays/events', async (route) => {
		bodies.push(route.request().postDataJSON() as Body);
		await route.fulfill({ status: 200, contentType: 'application/json', body: '{}' });
	});
	await page.route('**/api/library', (route) =>
		route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(ITEMS) })
	);
	await page.route('**/test-audio/**', () => {});
	await page.addInitScript(() => {
		HTMLMediaElement.prototype.play = function () {
			Object.defineProperty(this, 'paused', { value: false, configurable: true });
			Object.defineProperty(this, 'ended', { value: false, configurable: true });
			return Promise.resolve();
		};
		HTMLMediaElement.prototype.pause = function () {
			Object.defineProperty(this, 'paused', { value: true, configurable: true });
		};
	});
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);

	await selectRow(page, 'lib-coral', 'Coral Night');
	await selectRow(page, 'lib-pine', 'Pine Morning');
	await expect(page.getByTestId('audio-player')).toBeVisible();
	await playing(page);
	await expect.poll(() => bodies.map((b) => `${b.track_id}:${b.event}`)).toEqual(['lib-coral:start']);
	const first = bodies[0].playback_id;

	// Replacing the source ends the started playback with the captured old track.
	await page.getByTestId('player-next').click();
	await expect
		.poll(() => bodies.map((b) => `${b.track_id}:${b.event}`))
		.toEqual(['lib-coral:start', 'lib-coral:skip']);
	expect(bodies[1].playback_id).toBe(first);

	await playing(page);
	await expect.poll(() => bodies.length).toBe(3);
	expect(bodies[2]).toMatchObject({ track_id: 'lib-pine', event: 'start' });
	expect(bodies[2].playback_id).not.toBe(first);

	await page.getByTestId('audio-player').evaluate((el) => {
		Object.defineProperty(el, 'paused', { value: true, configurable: true });
		Object.defineProperty(el, 'ended', { value: true, configurable: true });
		el.dispatchEvent(new Event('pause'));
		el.dispatchEvent(new Event('ended'));
	});
	await expect.poll(() => bodies.map((b) => b.event)).toEqual(['start', 'skip', 'start', 'complete']);
	expect(bodies[3].playback_id).toBe(bodies[2].playback_id);
});
