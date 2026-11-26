import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { fixtureCookie, hasFixtureSession, test } from './helpers/private-music-fixture.js';

test('fresh invited Library results use the sort chosen during Refresh', async ({ page, context, origin }) => {
	const { library } = PRIVATE_MUSIC;
	const listener = 'synthetic-listener';
	const binding = 'A'.repeat(43);
	const verifierPath = '/v1/me/likes/session';
	const initial = [
		{ id: '00000000-0000-4000-8000-000000000001', title: 'Amber Shore' },
		{ id: '00000000-0000-4000-8000-000000000002', title: 'Willow Bay' }
	];
	// Send the fresh tracks in ascending order so only the new sort puts Zenith first.
	const fresh = [
		{ id: '00000000-0000-4000-8000-000000000003', title: 'Birch Lake' },
		{ id: '00000000-0000-4000-8000-000000000004', title: 'Zenith Hill' }
	];
	const releaseRefresh = Promise.withResolvers<void>();
	let listingReads = 0;

	await context.route('**/*', async (route) => {
		const request = route.request();
		const { origin: requestOrigin, pathname, search } = new URL(request.url());
		if (requestOrigin !== origin || request.method() !== 'GET') return route.abort();
		if (!search && (pathname === library.api || pathname === verifierPath)) {
			if (!hasFixtureSession(await request.headerValue('cookie'), listener)) return route.fulfill({ status: 401 });
			if (pathname === verifierPath) return route.fulfill({ json: { session_binding: binding } });
			const read = ++listingReads;
			if (read > 2) return route.fulfill({ status: 500 });
			if (read === 2) await releaseRefresh.promise;
			return route.fulfill({ headers: { 'x-nntune-session-binding': binding }, json: read === 1 ? initial : fresh });
		}
		if (pathname.startsWith('/api/') || pathname.startsWith('/v1/')) return route.fulfill({ status: 404 });
		return route.continue();
	});
	await context.addCookies([{ name: fixtureCookie, value: listener, url: origin, httpOnly: true }]);

	await page.goto(origin + library.path);
	const main = page.getByRole('main');
	const rows = main.getByTestId(library.item);
	const order = main.getByRole('combobox', { name: 'Order tracks', exact: true });
	const selectedOrder = order.locator('option:checked');
	const loading = main.getByRole('status').filter({ hasText: 'Loading your library…' });
	const expectTitles = async (first: string, second: string) => {
		await expect(rows).toHaveCount(2);
		await expect(rows.nth(0).getByRole('button', { name: first, exact: true })).toBeVisible();
		await expect(rows.nth(1).getByRole('button', { name: second, exact: true })).toBeVisible();
	};

	await expectTitles('Amber Shore', 'Willow Bay');
	await order.selectOption({ label: 'Title A–Z' });
	await expect(selectedOrder).toHaveText('Title A–Z');
	expect(listingReads).toBe(1);

	try {
		await Promise.all([
			page.waitForRequest(origin + library.api),
			main.getByRole('button', { name: library.refresh, exact: true }).click()
		]);
		await expect(loading).toBeVisible();
		await order.selectOption({ label: 'Title Z–A' });
		await expect(selectedOrder).toHaveText('Title Z–A');
		await expectTitles('Willow Bay', 'Amber Shore');
		await expect(loading).toBeVisible();
	} finally {
		releaseRefresh.resolve();
	}

	await expectTitles('Zenith Hill', 'Birch Lake');
	await expect(loading).toBeHidden();
	await expect(selectedOrder).toHaveText('Title Z–A');
	expect(listingReads).toBe(2);
});
