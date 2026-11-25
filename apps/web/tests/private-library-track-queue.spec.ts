import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { fixtureCookie, hasFixtureSession, observeMediaSources, test } from './helpers/private-music-fixture.js';

test('the invited Library queues the chosen same-title tracks after sorting', async ({ page, context, origin }) => {
	const { library, player } = PRIVATE_MUSIC;
	const listener = 'synthetic-listener';
	const binding = 'A'.repeat(43);
	const verifierPath = '/v1/me/likes/session';
	const title = 'Synthetic Echo';
	const maple = { id: '00000000-0000-4000-8000-000000000001', title, artist: 'Maple' };
	const noon = { id: '00000000-0000-4000-8000-000000000002', title: 'Synthetic Noon', artist: 'Birch' };
	const alder = { id: '00000000-0000-4000-8000-000000000003', title, artist: 'Alder' };
	const tracks = [noon, alder, maple];
	const playbackRequests: string[] = [];

	await context.route('**/*', async (route) => {
		const request = route.request();
		const { origin: requestOrigin, pathname, search } = new URL(request.url());
		if (pathname === player.plays || pathname.startsWith(`${player.plays}/`) || request.resourceType() === 'media') {
			playbackRequests.push(pathname);
			return route.abort();
		}
		if (requestOrigin !== origin || request.method() !== 'GET') return route.abort();
		if (!search && (pathname === library.api || pathname === verifierPath)) {
			if (!hasFixtureSession(await request.headerValue('cookie'), listener)) return route.fulfill({ status: 401 });
			return pathname === library.api
				? route.fulfill({ headers: { 'x-nntune-session-binding': binding }, json: tracks })
				: route.fulfill({ json: { session_binding: binding } });
		}
		if (pathname.startsWith('/api/') || pathname.startsWith('/v1/')) return route.fulfill({ status: 404 });
		return route.continue();
	});
	await context.addCookies([{ name: fixtureCookie, value: listener, url: origin, httpOnly: true }]);

	await page.goto(origin + library.path);
	const mediaSources = await observeMediaSources(page);
	const rows = page.getByTestId('library-list').getByTestId(library.item);
	const row = (id: string) => rows.and(page.locator(`[${library.id}="${id}"]`));
	const rowIds = () => rows.evaluateAll((nodes, attribute) => nodes.map((node) => node.getAttribute(attribute)), library.id);
	const queue = page.getByTestId(player.queueItem);
	const queueIds = () => queue.evaluateAll((nodes, attribute) => nodes.map((node) => node.getAttribute(attribute)), player.queueId);
	const order = page.getByRole('combobox', { name: 'Order tracks', exact: true });
	const mapleTitle = row(maple.id).getByRole('button', { name: `${title} — ${maple.artist}`, exact: true });
	const mapleSelection = row(maple.id).getByRole('checkbox');
	const alderSelection = row(alder.id).getByRole('checkbox');
	const selectionCount = page.getByTestId('selection-count');

	await expect.poll(rowIds).toEqual([noon.id, alder.id, maple.id]);
	await expect(mapleTitle).toBeVisible();
	await expect(row(alder.id).getByRole('button', { name: `${title} — ${alder.artist}`, exact: true })).toBeVisible();
	await alderSelection.check();
	await expect(selectionCount).toHaveText('1 selected');
	await expect(mapleSelection).not.toBeChecked();
	await mapleSelection.check();
	await expect(selectionCount).toHaveText('2 selected');
	await alderSelection.uncheck();
	await expect(selectionCount).toHaveText('1 selected');

	const menuName = `More actions for ${title}`;
	const add = page.getByRole('menuitem', { name: 'Add to queue', exact: true });
	await order.selectOption({ label: 'Title A–Z' });
	await expect.poll(rowIds).toEqual([alder.id, maple.id, noon.id]);
	await expect(queue).toHaveCount(0);
	// Invoke the second same-named menu so focus restoration must identify the specific button.
	await rows.nth(1).getByRole('button', { name: menuName, exact: true }).press('Enter');
	await expect(add).toBeVisible();
	await page.keyboard.press('a');
	await page.keyboard.press('Enter');
	await expect(row(maple.id).getByRole('button', { name: menuName, exact: true })).toBeFocused();
	await expect.poll(queueIds).toEqual([maple.id]);

	await order.selectOption({ label: 'Artist A–Z' });
	await expect.poll(rowIds).toEqual([alder.id, noon.id, maple.id]);
	await expect(mapleSelection).toBeChecked();
	await expect(alderSelection).not.toBeChecked();
	await rows.first().getByRole('button', { name: menuName, exact: true }).click();
	await add.click();
	// Queue order follows the add actions even though the Library shows Alder first.
	await expect.poll(queueIds).toEqual([maple.id, alder.id]);
	await expect(queue.nth(0)).toBeVisible();
	await expect(queue.nth(1)).toBeVisible();
	expect(await mediaSources.evaluate((sources) => [...sources].filter(Boolean))).toEqual([]);
	expect(playbackRequests).toEqual([]);
});
