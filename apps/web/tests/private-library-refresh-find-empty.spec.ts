import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { fixtureCookie, hasFixtureSession, test } from './helpers/private-music-fixture.js';

test('Refresh keeps Find active when its last invited Library match disappears', async ({
	page, context, origin
}) => {
	const { library } = PRIVATE_MUSIC;
	const listener = 'synthetic-listener';
	const binding = 'A'.repeat(43);
	const verifierPath = '/v1/me/likes/session';
	const match = { id: '00000000-0000-4000-8000-000000000001', title: 'Harbor Dawn' };
	const survivor = { id: '00000000-0000-4000-8000-000000000002', title: 'Field Dusk' };
	const snapshots = [[match, survivor], [survivor]];
	let listingReads = 0;
	await context.route('**/*', async (route) => {
		const request = route.request();
		const { origin: requestOrigin, pathname, search } = new URL(request.url());
		if (requestOrigin !== origin || request.method() !== 'GET') return route.abort();
		const listingIndex = pathname === library.api ? listingReads++ : undefined;
		if (!search && (listingIndex !== undefined || pathname === verifierPath)) {
			if (!hasFixtureSession(await request.headerValue('cookie'), listener)) {
				return route.fulfill({ status: 401 });
			}
			if (listingIndex === undefined) return route.fulfill({ json: { session_binding: binding } });
			const listing = snapshots[listingIndex];
			if (!listing) return route.fulfill({ status: 404 });
			return route.fulfill({ headers: { 'x-nntune-session-binding': binding }, json: listing });
		}
		if (pathname.startsWith('/api/') || pathname.startsWith('/v1/')) return route.fulfill({ status: 404 });
		return route.continue();
	});
	await context.addCookies([{ name: fixtureCookie, value: listener, url: origin, httpOnly: true }]);

	const main = page.getByRole('main');
	const rows = main.getByTestId(library.item);
	const row = (track: typeof match) => rows.and(page.locator(`[${library.id}="${track.id}"]`));
	const find = main.getByRole('searchbox', { name: 'Find in loaded tracks', exact: true });
	const scope = main.getByRole('combobox', { name: 'Search in', exact: true });
	const count = main.getByRole('status').and(main.getByTestId('library-find-count'));
	const empty = main.getByText('No title matches in your loaded tracks.', { exact: true });
	const clear = main.getByRole('button', { name: 'Clear search', exact: true });

	await page.goto(origin + library.path);
	await expect(rows).toHaveCount(2);
	await expect(row(survivor)).toBeVisible();
	await scope.selectOption('title');
	await find.fill('harbor');
	await expect(count).toContainText('Showing 1 of 2');
	await expect(row(match)).toBeVisible();
	await expect(row(survivor)).toHaveCount(0);
	expect(listingReads).toBe(1);

	const [response] = await Promise.all([
		page.waitForResponse(origin + library.api),
		main.getByRole('button', { name: library.refresh, exact: true }).click()
	]);
	expect(response.status()).toBe(200);
	await expect(count).toContainText('Showing 0 of 1');
	await expect(empty).toBeVisible();
	await expect(find).toHaveValue('harbor');
	await expect(scope).toHaveValue('title');
	await expect(rows).toHaveCount(0);
	await expect(main.getByText('Your library is empty.', { exact: true })).toHaveCount(0);

	// The second Clear search is the recovery action beside the no-matches message.
	await expect(clear).toHaveCount(2);
	await clear.last().press('Enter');
	await expect(find).toHaveValue('');
	await expect(find).toBeFocused();
	await expect(scope).toHaveValue('title');
	await expect(empty).toHaveCount(0);
	await expect(count).toContainText('Showing 1 of 1');
	await expect(rows).toHaveCount(1);
	await expect(row(survivor).getByRole('button', { name: survivor.title, exact: true })).toBeVisible();
	await expect(row(match)).toHaveCount(0);
	expect(listingReads).toBe(2);
});
