import { expect, test, type Page } from '@playwright/test';
import { contextPath, eventsPath, watchListenContext } from '../fixtures/listen-context.js';

/**
 * Fixture-only library integration for listen-event telemetry. Fixtures follow
 * library-repeat-end-policy.spec.ts: route-mocked `/api/library`, pending
 * `/test-audio/**`, doubled media play/pause, synthetic media events. The real
 * route injects simulated grant IDs; `/v1/plays/events` is fulfilled in-test.
 */
type Body = { playback_id: string; track_id: string; event: string; context: string };

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

const original = 'a'.repeat(43);
const replacement = 'b'.repeat(43);

async function mountLibrary(page: Page) {
	await page.route('**/api/library', (route) => route.fulfill({ json: ITEMS }));
	await page.route('**/test-audio/**', () => {});
	await watchListenContext(page);
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
	await expect(page.locator('html')).toHaveAttribute('data-history-context-read', 'true');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await selectRow(page, 'lib-coral', 'Coral Night');
	await selectRow(page, 'lib-pine', 'Pine Morning');
	await expect(page.getByTestId('audio-player')).toBeVisible();
}

test('library records a terminal against the start for the same audio element', async ({ page }) => {
	const bodies: Body[] = [];
	await page.route(contextPath, (route) => route.fulfill({ json: { context: original } }));
	await page.route(eventsPath, (route) => {
		bodies.push(route.request().postDataJSON() as Body);
		return route.fulfill({ json: {} });
	});
	await mountLibrary(page);
	await playing(page);
	await expect.poll(() => bodies.map((b) => b.event)).toEqual(['start']);
	await page.getByTestId('player-next').click();
	await expect.poll(() => bodies.map((b) => `${b.track_id}:${b.event}`)).toEqual([
		'lib-coral:start', 'lib-coral:skip'
	]);
	expect(bodies[1].playback_id).toBe(bodies[0].playback_id);

	await playing(page);
	await expect.poll(() => bodies.length).toBe(3);
	await page.getByTestId('audio-player').evaluate((el) => {
		Object.defineProperty(el, 'ended', { value: true, configurable: true });
		el.dispatchEvent(new Event('ended'));
	});
	await expect.poll(() => bodies.map((b) => b.event)).toEqual(['start', 'skip', 'start', 'complete']);
	expect(bodies[3].playback_id).toBe(bodies[2].playback_id);
});

test('leaving the library records one skip for the started playback', async ({ page }) => {
	const bodies: Body[] = [];
	await page.route(contextPath, (route) => route.fulfill({ json: { context: original } }));
	await page.route(eventsPath, (route) => {
		bodies.push(route.request().postDataJSON() as Body);
		return route.fulfill({ json: {} });
	});
	await mountLibrary(page);
	await playing(page);
	await expect.poll(() => bodies.map((b) => b.event)).toEqual(['start']);
	await page.getByRole('link', { name: 'NNTune', exact: true }).click();
	await expect(page).toHaveURL(/\/home$/);
	await expect.poll(() => bodies.map((b) => b.event)).toEqual(['start', 'skip']);
	expect(bodies[1]).toEqual({ ...bodies[0], event: 'skip' });
});

test('library keeps a pending start bound when no auth notification arrives', async ({ page }) => {
	let activeContext = original;
	let contextLoads = 0;
	const received: Body[] = [];
	const cookies: (string | undefined)[] = [];
	const setSession = (value: string) => page.context().addCookies([
		{ name: 'fixture_session', value, url: new URL(page.url()).origin, httpOnly: true }
	]);
	await page.route(contextPath, (route) => {
		contextLoads++;
		return route.fulfill({ json: { context: activeContext } });
	});
	await page.route(eventsPath, async (route) => {
		const body = route.request().postDataJSON() as Body;
		received.push(body);
		cookies.push((await route.request().allHeaders()).cookie);
		if (received.length === 1) {
			// Synthetic session switch: the cookie and the context endpoint both change.
			await setSession('second');
			activeContext = replacement;
			return route.fulfill({ json: {}, status: 503 });
		}
		return route.fulfill({ json: {}, status: body.context === activeContext ? 200 : 401 });
	});
	await mountLibrary(page);
	await setSession('first');
	const refused = page.waitForResponse((response) =>
		response.url().endsWith('/v1/plays/events') && response.status() === 401
	);
	await playing(page);
	await refused;
	expect(received.map((body) => body.context)).toEqual([original, original]);
	expect(received[0]).toEqual(received[1]);
	// The retry carries the new cookie but still the original JSON binding.
	expect(cookies[0]).toContain('fixture_session=first');
	expect(cookies[1]).toContain('fixture_session=second');
	expect(contextLoads).toBe(1);
	await expect(page.getByTestId('audio-player')).toBeVisible();
});
