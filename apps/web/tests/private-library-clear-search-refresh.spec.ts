import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { fixtureCookie, hasFixtureSession, test } from './helpers/private-music-fixture.js';

test('clearing Find during an invited Library refresh shows the full refreshed catalogue', async ({
	page, context, origin
}) => {
	const { library } = PRIVATE_MUSIC;
	const listener = 'synthetic-listener';
	const binding = 'A'.repeat(43);
	const verifierPath = '/v1/me/likes/session';
	const match = { id: '00000000-0000-4000-8000-000000000001', title: 'Harbor Dawn' };
	const initial = [
		match,
		{ id: '00000000-0000-4000-8000-000000000002', title: 'Field Dawn' }
	];
	const refreshed = [
		{ id: '00000000-0000-4000-8000-000000000003', title: 'Harbor Dusk' },
		{ id: '00000000-0000-4000-8000-000000000004', title: 'Field Dusk' }
	];
	const refreshStarted = Promise.withResolvers<void>();
	const refreshReply = Promise.withResolvers<typeof refreshed>();
	let listings = 0;

	await context.route('**/*', async (route) => {
		const request = route.request();
		const { origin: requestOrigin, pathname, search } = new URL(request.url());
		if (requestOrigin !== origin || request.method() !== 'GET') return route.abort();
		if (!search && (pathname === library.api || pathname === verifierPath)) {
			if (!hasFixtureSession(await request.headerValue('cookie'), listener)) return route.fulfill({ status: 401 });
			if (pathname === verifierPath) return route.fulfill({ json: { session_binding: binding } });
			listings += 1;
			if (listings > 2) return route.fulfill({ status: 500 });
			if (listings === 2) refreshStarted.resolve();
			return route.fulfill({
				headers: { 'x-nntune-session-binding': binding },
				json: listings === 1 ? initial : await refreshReply.promise
			});
		}
		if (pathname.startsWith('/api/') || pathname.startsWith('/v1/')) return route.fulfill({ status: 404 });
		return route.continue();
	});
	await context.addCookies([{ name: fixtureCookie, value: listener, url: origin, httpOnly: true }]);

	await page.goto(origin + library.path);
	const main = page.getByRole('main');
	const rows = main.getByTestId(library.item);
	const find = main.getByRole('searchbox', { name: 'Find in loaded tracks', exact: true });
	const loading = main.getByRole('status').filter({ hasText: 'Loading your library…' });
	const expectTracks = async (tracks: typeof initial) => {
		await expect(rows).toHaveCount(tracks.length);
		for (const { id, title } of tracks) {
			const row = rows.and(page.locator(`[${library.id}="${id}"]`));
			await expect(row.getByRole('button', { name: title, exact: true })).toBeVisible();
		}
	};

	await expectTracks(initial);
	await find.fill('harbor');
	await expectTracks([match]);
	expect(listings).toBe(1);

	try {
		await main.getByRole('button', { name: library.refresh, exact: true }).click();
		await refreshStarted.promise;
		await expect(loading).toBeVisible();
		await main.getByRole('button', { name: 'Clear search', exact: true }).click();
		await expect(find).toHaveValue('');
		await expectTracks(initial);
		await expect(loading).toBeVisible();
	} finally {
		refreshReply.resolve(refreshed);
	}

	// The response arrives after the clear. Its nonmatching row catches a restored old filter.
	await expectTracks(refreshed);
	await expect(loading).toBeHidden();
	await expect(find).toHaveValue('');
	expect(listings).toBe(2);
});
