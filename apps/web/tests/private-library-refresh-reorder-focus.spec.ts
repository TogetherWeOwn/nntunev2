import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { fixtureCookie, hasFixtureSession, observeMediaSources, test } from './helpers/private-music-fixture.js';

test('an invited Library refresh keeps keyboard focus on the reordered track', async ({ page, context, origin }) => {
	const { library, player } = PRIVATE_MUSIC;
	const listener = 'synthetic-listener';
	const binding = 'A'.repeat(43);
	const verifierPath = '/v1/me/likes/session';
	const neighbor = { id: '00000000-0000-4000-8000-000000000001', title: 'Synthetic Prelude' };
	const moved = { id: '00000000-0000-4000-8000-000000000002', title: 'Synthetic Finale' };
	const reply = Promise.withResolvers<void>();
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
			const listing = ++listings;
			if (listing > 2) return route.fulfill({ status: 404 });
			if (listing === 2) await reply.promise;
			return route.fulfill({
				headers: { 'x-nntune-session-binding': binding },
				json: listing === 1 ? [moved, neighbor] : [neighbor, moved]
			});
		}
		if (path.startsWith('/api/') || path.startsWith('/v1/')) return route.fulfill({ status: 404 });
		return route.continue();
	});
	await context.addCookies([{ name: fixtureCookie, value: listener, url: origin, httpOnly: true }]);
	await page.goto(origin + library.path);
	const sources = await observeMediaSources(page);
	const rows = page.getByTestId('library-list').getByTestId(library.item);
	const movedRow = rows.and(page.locator(`[${library.id}="${moved.id}"]`));
	const more = movedRow.getByRole('button', { name: `More actions for ${moved.title}`, exact: true });
	const play = movedRow.getByRole('button', { name: moved.title, exact: true });
	const refresh = page.getByRole('button', { name: library.refresh, exact: true });
	await expect(rows).toHaveCount(2);
	await expect(rows.nth(0)).toHaveAttribute(library.id, moved.id);
	await expect(rows.nth(1)).toHaveAttribute(library.id, neighbor.id);

	const requested = page.waitForRequest(`${origin}${library.api}`);
	const response = page.waitForResponse(`${origin}${library.api}`);
	try {
		await refresh.press('Enter');
		await requested;
		await expect(refresh).toBeFocused();
		for (let tabs = 0; tabs < 24 && !(await more.evaluate((element) => element.matches(':focus'))); tabs++) {
			await page.keyboard.press('Shift+Tab');
		}
		await expect(more).toBeFocused();
	} finally {
		reply.resolve();
	}

	expect((await response).status()).toBe(200);
	await expect(rows).toHaveCount(2);
	await expect(rows.nth(0)).toHaveAttribute(library.id, neighbor.id);
	await expect(rows.nth(1)).toHaveAttribute(library.id, moved.id);
	await expect(more).toBeFocused();
	await page.keyboard.press('Shift+Tab');
	await expect(play).toBeFocused();
	await page.keyboard.press('Tab');
	await expect(more).toBeFocused();
	await expect(page.getByTestId(player.queueItem)).toHaveCount(0);
	expect(playbackRequests).toEqual([]);
	expect(await sources.evaluate((seen) => [...seen])).toEqual([]);
	expect(listings).toBe(2);
});
