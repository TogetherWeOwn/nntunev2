import { randomUUID } from 'node:crypto';
import { expect, test } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { fixtureCookie, hasFixtureSession } from './helpers/private-music-fixture.js';

test.use({ serviceWorkers: 'block' });

test('the invited Library restores all tracks and keeps the selection after changing Show', async ({
	page,
	context,
	baseURL
}) => {
	const { protocol, hostname, origin } = new URL(baseURL!);
	if (protocol !== 'http:' || !['localhost', '127.0.0.1', '[::1]'].includes(hostname)) {
		throw new Error('The invited Library fixture requires a loopback HTTP server');
	}
	const { library } = PRIVATE_MUSIC;
	const listener = 'synthetic-listener';
	const binding = 'A'.repeat(43);
	const verifierPath = '/v1/me/likes/session';
	const [first, middle, last] = ['Quartz', 'Agate', 'Jasper'].map((title) => ({
		id: randomUUID(), title
	}));
	const catalogue = [first, middle, last];
	let listingReads = 0;

	await context.route('**/*', async (route) => {
		const request = route.request();
		const { origin: requestOrigin, pathname } = new URL(request.url());
		if (requestOrigin !== origin || request.method() !== 'GET') return route.abort();
		if (pathname === library.api) listingReads++;
		if (pathname === library.api || pathname === verifierPath) {
			if (!hasFixtureSession(await request.headerValue('cookie'), listener)) {
				return route.fulfill({ status: 401 });
			}
			return pathname === library.api
				? route.fulfill({ headers: { 'x-nntune-session-binding': binding }, json: catalogue })
				: route.fulfill({ json: { session_binding: binding } });
		}
		if (pathname.startsWith('/api/') || pathname.startsWith('/v1/')) {
			return route.fulfill({ status: 404 });
		}
		return route.continue();
	});
	await context.addCookies([{ name: fixtureCookie, value: listener, url: origin, httpOnly: true }]);

	const rows = page.getByTestId(library.item);
	const show = page.getByRole('combobox', { name: 'Show', exact: true });
	const selectionCount = page.getByTestId('selection-count');
	const checkbox = (track: typeof first) => rows.getByRole('checkbox', { name: `Select ${track.title}`, exact: true });
	const expectCatalogue = async (tracks: typeof catalogue) => {
		await expect(rows).toHaveCount(tracks.length);
		for (const [index, track] of tracks.entries()) {
			const row = rows.nth(index);
			await expect(row).toHaveAttribute(library.id, track.id);
			await expect(row.getByRole('button', { name: track.title, exact: true })).toBeVisible();
		}
	};

	await page.goto(origin + library.path);
	await expectCatalogue(catalogue);
	await expect(show.locator('option:checked')).toHaveText('All tracks');
	await expect(selectionCount).toHaveText('0 selected');
	await checkbox(last).check();
	await checkbox(first).check();
	await expect(selectionCount).toHaveText('2 selected');

	await show.selectOption({ label: 'Selected only' });
	await expectCatalogue([first, last]);
	await expect(selectionCount).toHaveText('2 selected');
	await expect(checkbox(first)).toBeChecked();
	await expect(checkbox(last)).toBeChecked();

	await show.selectOption({ label: 'All tracks' });
	await expectCatalogue(catalogue);
	await expect(selectionCount).toHaveText('2 selected');
	await expect(checkbox(first)).toBeChecked();
	await expect(checkbox(middle)).not.toBeChecked();
	await expect(checkbox(last)).toBeChecked();
	expect(listingReads).toBe(1);
});
