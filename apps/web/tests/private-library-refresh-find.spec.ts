import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { fixtureCookie, hasFixtureSession, test } from './helpers/private-music-fixture.js';

test('Refresh updates the invited Library while Find stays active', async ({ page, context, origin }) => {
	const { library } = PRIVATE_MUSIC;
	const listener = 'synthetic-listener';
	const binding = 'A'.repeat(43);
	const verifierPath = '/v1/me/likes/session';
	const removed = { id: '00000000-0000-4000-8000-000000000001', title: 'Harbor Dawn' };
	const added = { id: '00000000-0000-4000-8000-000000000002', title: 'Harbor Dusk' };
	const unrelated = { id: '00000000-0000-4000-8000-000000000003', title: 'Field Echo' };
	let listing = [removed, unrelated];
	let listingReads = 0;

	await context.route('**/*', async (route) => {
		const request = route.request();
		const { origin: requestOrigin, pathname, search } = new URL(request.url());
		if (requestOrigin !== origin || request.method() !== 'GET') return route.abort();
		if (!search && (pathname === library.api || pathname === verifierPath)) {
			if (!hasFixtureSession(await request.headerValue('cookie'), listener)) return route.fulfill({ status: 401 });
			if (pathname === verifierPath) return route.fulfill({ json: { session_binding: binding } });
			listingReads += 1;
			return route.fulfill({ headers: { 'x-nntune-session-binding': binding }, json: listing });
		}
		if (pathname.startsWith('/api/') || pathname.startsWith('/v1/')) return route.fulfill({ status: 404 });
		return route.continue();
	});
	await context.addCookies([{ name: fixtureCookie, value: listener, url: origin, httpOnly: true }]);

	await page.goto(origin + library.path);
	const main = page.getByRole('main');
	const rows = main.getByTestId(library.item);
	const track = ({ id, title }: typeof added) => rows.and(page.locator(`[${library.id}="${id}"]`))
		.getByRole('button', { name: title, exact: true });
	const find = main.getByRole('searchbox', { name: 'Find in loaded tracks', exact: true });
	const query = 'harbor';
	await expect(rows).toHaveCount(2);
	await expect(track(removed)).toBeVisible();
	await expect(track(unrelated)).toBeVisible();
	await find.fill(query);
	await expect(rows).toHaveCount(1);
	await expect(track(removed)).toBeVisible();
	expect(listingReads).toBe(1);

	listing = [added, unrelated];
	const [response] = await Promise.all([
		page.waitForResponse(origin + library.api),
		main.getByRole('button', { name: library.refresh, exact: true }).click()
	]);
	expect(response.status()).toBe(200);
	await expect(track(added)).toBeVisible();
	await expect(rows).toHaveCount(1);
	await expect(find).toHaveValue(query);

	await main.getByRole('button', { name: 'Clear search', exact: true }).click();
	await expect(find).toHaveValue('');
	await expect(rows).toHaveCount(2);
	await expect(track(added)).toBeVisible();
	await expect(track(unrelated)).toBeVisible();
	expect(listingReads).toBe(2);
});
