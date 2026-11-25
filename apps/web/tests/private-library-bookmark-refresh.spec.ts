import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { fixtureCookie, hasFixtureSession, observeMediaSources, test } from './helpers/private-music-fixture.js';

test('the invited Library reopens a same-title track bookmark after refresh', async ({ page, context, origin }) => {
	const { library, player } = PRIVATE_MUSIC;
	const listener = 'synthetic-bookmark-listener';
	const binding = 'A'.repeat(43);
	const verifierPath = '/v1/me/likes/session';
	const title = 'Synthetic Echo';
	const other = { id: '00000000-0000-4000-8000-000000000001', title, artist: 'Alder' };
	const saved = { id: '00000000-0000-4000-8000-000000000002', title, artist: 'Maple' };
	const libraryUrl = origin + library.path;
	const bookmark = `${libraryUrl}?track=${saved.id}&t=0`;
	const playbackRequests: string[] = [];
	let listings = 0;

	await context.route('**/*', async (route) => {
		const request = route.request();
		const url = new URL(request.url());
		const path = url.pathname;
		if (path === player.plays || path.startsWith(`${player.plays}/`) || request.resourceType() === 'media') {
			playbackRequests.push(path);
			return route.abort();
		}
		if (url.origin !== origin || request.method() !== 'GET') return route.abort();
		if (!url.search && (path === library.api || path === verifierPath)) {
			if (!hasFixtureSession(await request.headerValue('cookie'), listener)) return route.fulfill({ status: 401 });
			if (path === verifierPath) return route.fulfill({ json: { session_binding: binding } });
			listings += 1;
			if (listings > 2) return route.fulfill({ status: 500 });
			return route.fulfill({
				headers: { 'x-nntune-session-binding': binding },
				json: listings === 1 ? [other, saved] : [saved, other]
			});
		}
		if (path.startsWith('/api/') || path.startsWith('/v1/')) return route.fulfill({ status: 404 });
		return route.continue();
	});
	await context.addCookies([{ name: fixtureCookie, value: listener, url: origin, httpOnly: true }]);

	await page.goto(libraryUrl);
	const mediaSources = await observeMediaSources(page);
	const rows = page.getByTestId('library-list').getByTestId(library.item);
	const rowIds = () => rows.evaluateAll((nodes, attribute) => nodes.map((node) => node.getAttribute(attribute)), library.id);
	const queue = page.getByTestId(player.queueItem);
	const current = page.locator(player.currentQueueItem);
	await expect.poll(rowIds).toEqual([other.id, saved.id]);
	for (const track of [other, saved]) {
		await expect(rows.getByRole('button', { name: `${title} — ${track.artist}`, exact: true })).toBeVisible();
	}

	// Follow the same stored URL twice through normal navigation while keeping the player mounted.
	await page.evaluate(({ bookmark, libraryUrl }) => {
		for (const { href, label } of [
			{ href: bookmark, label: 'Open saved track bookmark' },
			{ href: libraryUrl, label: 'Return to Library' }
		]) {
			const link = document.createElement('a');
			link.href = href;
			link.textContent = label;
			document.body.append(link);
		}
	}, { bookmark, libraryUrl });
	const openBookmark = page.getByRole('link', { name: 'Open saved track bookmark', exact: true });
	const expectBookmarkedTrack = async () => {
		await openBookmark.click();
		await expect(page).toHaveURL(bookmark);
		await expect(queue).toHaveCount(1);
		await expect(current).toBeVisible();
		await expect(current).toHaveAttribute(player.queueId, saved.id);
	};

	await expect(queue).toHaveCount(0);
	await expectBookmarkedTrack();
	await page.getByRole('link', { name: 'Return to Library', exact: true }).click();
	await expect(page).toHaveURL(libraryUrl);
	await queue.getByRole('button', { name: `More actions for ${title}`, exact: true }).click();
	await page.getByRole('menuitem', { name: 'Remove from queue', exact: true }).click();
	await expect(queue).toHaveCount(0);
	expect(listings).toBe(1);

	await page.getByRole('button', { name: library.refresh, exact: true }).click();
	await expect.poll(rowIds).toEqual([saved.id, other.id]);
	await expect(queue).toHaveCount(0);
	await expectBookmarkedTrack();
	expect(listings).toBe(2);
	expect(await mediaSources.evaluate((sources) => [...sources].filter(Boolean))).toEqual([]);
	expect(playbackRequests).toEqual([]);
});
