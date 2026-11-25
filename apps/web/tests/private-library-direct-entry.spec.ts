import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { fixtureCookie, hasFixtureSession, observeMediaSources, test } from './helpers/private-music-fixture.js';

test('a signed-in invitee can open Library directly and queue a track without playback', async ({
	page, context, origin
}) => {
	const { library, player } = PRIVATE_MUSIC;
	const listener = 'synthetic-listener';
	const binding = 'A'.repeat(43);
	const track = { id: '00000000-0000-4000-8000-000000000001', title: 'Synthetic Prelude', duration_ms: 14_000 };
	const verifierPath = '/v1/me/likes/session';
	const visitedPaths = new Set<string>();
	const playbackRequests: string[] = [];
	page.on('framenavigated', (frame) => {
		if (frame === page.mainFrame()) visitedPaths.add(new URL(frame.url()).pathname);
	});
	await context.route('**/*', async (route) => {
		const request = route.request();
		const url = new URL(request.url());
		const path = url.pathname;
		if (path === player.plays || path.startsWith(`${player.plays}/`) || request.resourceType() === 'media') {
			playbackRequests.push(path);
			return route.abort();
		}
		if (url.origin !== origin || request.method() !== 'GET') return route.abort();
		if (path === library.api || path === verifierPath) {
			if (!hasFixtureSession(await request.headerValue('cookie'), listener)) return route.fulfill({ status: 401 });
			return path === library.api
				? route.fulfill({ headers: { 'x-nntune-session-binding': binding }, json: [track] })
				: route.fulfill({ json: { session_binding: binding } });
		}
		if (path.startsWith('/api/') || path.startsWith('/v1/')) return route.fulfill({ status: 404 });
		return route.continue();
	});
	await context.addCookies([{ name: fixtureCookie, value: listener, url: origin, httpOnly: true }]);

	const libraryUrl = origin + library.path;
	const response = await page.goto(libraryUrl);
	expect(response?.status()).toBe(200);
	const documentTimeOrigin = await page.evaluate(() => performance.timeOrigin);
	const mediaSources = await observeMediaSources(page);
	const rows = page.getByTestId(library.item);
	await expect(rows).toHaveCount(1);
	await expect(rows).toHaveAttribute(library.id, track.id);
	await expect(rows.getByRole('button', { name: track.title, exact: true })).toBeVisible();

	const queue = page.getByTestId(player.queueItem);
	await expect(queue).toHaveCount(0);
	await rows.getByRole('button', { name: `More actions for ${track.title}`, exact: true }).press('Enter');
	const add = page.getByRole('menuitem', { name: 'Add to queue', exact: true });
	await expect(add).toBeVisible();
	await page.keyboard.press('a');
	await expect(add).toBeFocused();
	await page.keyboard.press('Enter');

	await expect(queue).toHaveCount(1);
	await expect(queue).toBeVisible();
	await expect(queue).toHaveAttribute(player.queueId, track.id);
	await expect(page.getByTestId('queue-announcement')).toHaveText('Added 1 track (0 already queued)');
	await expect(page.locator('audio[src]:not([src=""]), audio source[src]:not([src=""])')).toHaveCount(0);
	await expect(page).toHaveURL(libraryUrl);
	expect([...visitedPaths]).toEqual([library.path]);
	expect(await page.evaluate(() => performance.timeOrigin)).toBe(documentTimeOrigin);
	expect(await mediaSources.evaluate((sources) => [...sources].filter(Boolean))).toEqual([]);
	expect(playbackRequests).toEqual([]);
});
