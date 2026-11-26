import { expect, type Page } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { fixtureCookie, hasFixtureSession, test } from './helpers/private-music-fixture.js';

const { library, player } = PRIVATE_MUSIC;
const verifierPath = '/v1/me/likes/session';
const listener = { account: 'synthetic-listener', binding: 'A'.repeat(43) };
const nextListener = { account: 'synthetic-next-listener', binding: 'E'.repeat(43) };
const first = { id: '00000000-0000-4000-8000-000000000001', title: 'First catalogue track' };
const selected = { id: '00000000-0000-4000-8000-000000000002', title: 'Selected catalogue track' };
const added = { id: '00000000-0000-4000-8000-000000000003', title: 'New catalogue track' };

type Invitee = typeof listener;
type Track = typeof first;
type Listing = Track[] | 503;

async function mockCatalogue(
	page: Page,
	origin: string,
	readListing: (invitee: Invitee) => Listing | Promise<Listing>,
	invitees = [listener]
) {
	const context = page.context();
	const listingAccounts: (string | undefined)[] = [];
	await context.route('**/*', async (route) => {
		const request = route.request();
		const url = new URL(request.url());
		if (url.origin !== origin || request.method() !== 'GET') return route.abort();
		if (!url.search && (url.pathname === library.api || url.pathname === verifierPath)) {
			const cookie = await request.headerValue('cookie');
			const invitee = invitees.find(({ account }) => hasFixtureSession(cookie, account));
			if (url.pathname === library.api) listingAccounts.push(invitee?.account);
			if (!invitee) return route.fulfill({ status: 401 });
			if (url.pathname === verifierPath) return route.fulfill({ json: { session_binding: invitee.binding } });
			const listing = await readListing(invitee);
			if (listing === 503) return route.fulfill({ status: 503, json: { code: 'unavailable', retryable: true } });
			return route.fulfill({ headers: { 'x-nntune-session-binding': invitee.binding }, json: listing });
		}
		if (url.pathname.startsWith('/api/') || url.pathname.startsWith('/v1/')) return route.fulfill({ status: 404 });
		return route.continue();
	});

	return {
		listingAccounts,
		signIn: (invitee = listener) => context.addCookies([
			{ name: fixtureCookie, value: invitee.account, url: origin, httpOnly: true }
		])
	};
}

function row(page: Page, track: Track) {
	return page.getByTestId(library.item).and(page.locator(`[${library.id}="${track.id}"]`));
}

async function expectCatalogue(page: Page, tracks: Track[]) {
	await expect(page.getByTestId(library.item)).toHaveCount(tracks.length);
	for (const track of tracks) {
		await expect(row(page, track).locator(library.select)).toContainText(track.title);
		await expect(row(page, track)).toBeVisible();
	}
}

test('first-load Retry keeps keyboard focus on an available Library action', async ({ page, origin }) => {
	const failedRetry = Promise.withResolvers<Listing>();
	let listing: Listing | Promise<Listing> = 503;
	const fixture = await mockCatalogue(page, origin, () => listing);
	await fixture.signIn();
	const main = page.getByRole('main');
	const retry = main.getByRole('button', { name: 'Retry', exact: true });
	const refresh = main.getByRole('button', { name: library.refresh, exact: true });
	const loading = main.getByRole('status').filter({ hasText: /^Loading your library…$/ });
	const alert = main.getByRole('alert');
	const listingUrl = origin + library.api;

	try {
		await page.goto(origin + library.path);
		await expect(alert).toHaveText('Library is not reachable right now.');
		await expectCatalogue(page, []);
		await expect(refresh).toHaveCount(0);
		await main.getByRole('button', { name: 'Artists', exact: true }).focus();
		await page.keyboard.press('Tab');
		await expect(retry).toBeFocused();

		listing = failedRetry.promise;
		await page.keyboard.press('Enter');
		await expect(loading).toBeVisible();
		await expect(retry).toHaveCount(0);
		failedRetry.resolve(503);
		await expect(alert).toHaveText('Library is not reachable right now.');
		await expectCatalogue(page, []);
		await expect(retry).toBeFocused();

		listing = [first];
		const [response] = await Promise.all([
			page.waitForResponse(listingUrl),
			page.keyboard.press('Enter')
		]);
		expect(response.status()).toBe(200);
		await expectCatalogue(page, [first]);
		await expect(alert).toHaveCount(0);
		await expect(retry).toHaveCount(0);
		await expect(refresh).toBeEnabled();
		await expect(refresh).toBeFocused();
		expect(fixture.listingAccounts).toEqual([listener.account, listener.account, listener.account]);
	} finally {
		failedRetry.resolve(503);
	}
});

for (const active of ['body', 'Density'] as const) {
	test(`ordinary first load preserves focus on ${active}`, async ({ page, origin }) => {
		const initial = Promise.withResolvers<Track[]>();
		const fixture = await mockCatalogue(page, origin, () => initial.promise);
		await fixture.signIn();
		const loading = page.getByRole('status').filter({ hasText: /^Loading your library…$/ });
		const focus = active === 'body' ? page.locator('body') : page.getByLabel(active, { exact: true });
		try {
			await page.goto(origin + library.path);
			await expect(loading).toBeVisible();
			if (active === 'Density') await focus.focus();
			await expect(focus).toBeFocused();
			initial.resolve([first]);
			await expectCatalogue(page, [first]);
			await expect(loading).toHaveCount(0);
			await expect(focus).toBeFocused();
			expect(fixture.listingAccounts).toEqual([listener.account]);
		} finally {
			initial.resolve([]);
		}
	});
}

test('Retry after a first-load 503 reads the current invited Library', async ({ page, origin }) => {
	let firstRead = true;
	const fixture = await mockCatalogue(page, origin, (invitee) => {
		if (firstRead) {
			firstRead = false;
			return 503;
		}
		return [invitee === listener ? first : added];
	}, [listener, nextListener]);
	await fixture.signIn();
	const listingUrl = origin + library.api;
	const [initial] = await Promise.all([page.waitForResponse(listingUrl), page.goto(origin + library.path)]);
	expect(initial.status()).toBe(503);
	const documentTimeOrigin = await page.evaluate(() => performance.timeOrigin);
	const alert = page.getByRole('alert');
	const retry = page.getByRole('button', { name: 'Retry', exact: true });
	await expect(alert).toHaveText('Library is not reachable right now.');
	await expectCatalogue(page, []);
	expect(fixture.listingAccounts).toEqual([listener.account]);

	// Replace the synthetic session while the failed Library document is still open.
	await fixture.signIn(nextListener);
	const [retried, verification] = await Promise.all([
		page.waitForResponse(listingUrl),
		page.waitForResponse(origin + verifierPath),
		retry.click()
	]);
	expect(retried.status()).toBe(200);
	expect(verification.status()).toBe(200);
	expect(await verification.json()).toEqual({ session_binding: nextListener.binding });
	await expectCatalogue(page, [added]);
	await expect(alert).toHaveCount(0);
	await expect(retry).toHaveCount(0);
	expect(fixture.listingAccounts).toEqual([listener.account, nextListener.account]);
	expect(await page.evaluate(() => performance.timeOrigin)).toBe(documentTimeOrigin);
});

test('Selected only survives a failed invited Library refresh and successful Retry', async ({ page, origin }) => {
	const updatedSelected = { ...selected, title: 'Updated selected track' };
	const updatedFirst = { ...first, title: 'Updated unselected track' };
	let listing: Listing = [first, selected];
	const fixture = await mockCatalogue(page, origin, () => listing);
	await fixture.signIn();

	const main = page.getByRole('main');
	const show = main.getByRole('combobox', { name: 'Show', exact: true });
	const count = main.getByRole('status').and(main.getByTestId('library-find-count'));
	const stale = main.getByTestId('library-stale-badge');
	const refresh = main.getByRole('button', { name: library.refresh, exact: true });
	const retry = main.getByRole('button', { name: 'Retry', exact: true });
	const checkbox = (track: Track) => row(page, track).getByRole('checkbox', { name: `Select ${track.title}`, exact: true });
	const expectFiltered = async (track: Track) => {
		await expect(show).toHaveValue('selected');
		await expect(count).toHaveText(/^Showing 1 of 2 tracks?$/);
		await expectCatalogue(page, [track]);
		await expect(checkbox(track)).toBeChecked();
	};

	await page.goto(origin + library.path);
	await expectCatalogue(page, listing);
	await checkbox(selected).check();
	await show.selectOption({ label: 'Selected only' });
	await expectFiltered(selected);

	listing = 503;
	const [failed] = await Promise.all([page.waitForResponse(origin + library.api), refresh.click()]);
	expect(failed.status()).toBe(503);
	await expect(stale).toBeVisible();
	await expectFiltered(selected);

	listing = [updatedFirst, updatedSelected];
	const [retried] = await Promise.all([page.waitForResponse(origin + library.api), retry.click()]);
	expect(retried.status()).toBe(200);
	await expectFiltered(updatedSelected);
	await expect(stale).toBeHidden();
	await expect(refresh).toBeVisible();

	await show.selectOption({ label: 'All tracks' });
	await expectCatalogue(page, listing);
	await expect(checkbox(updatedFirst)).not.toBeChecked();
	expect(fixture.listingAccounts).toEqual([listener.account, listener.account, listener.account]);
});

test('keyboard Refresh and Retry keep focus and update one live status', async ({ page, origin }) => {
	const failed = Promise.withResolvers<503>();
	const recovered = Promise.withResolvers<Track[]>();
	const replies = [[first], failed.promise, recovered.promise];
	const fixture = await mockCatalogue(page, origin, () => replies.shift() ?? 503);
	await fixture.signIn();
	const main = page.getByRole('main');
	const refresh = main.getByRole('button', { name: 'Refresh', exact: true });
	const retry = main.getByRole('button', { name: 'Retry', exact: true });
	const loading = main.getByRole('status').filter({ hasText: /^Loading your library…$/ });
	const stale = main.getByRole('status').and(main.getByTestId('library-stale-badge'));
	const refreshed = main.getByRole('status').filter({ hasText: /^Library refreshed\.$/ });

	try {
		await page.goto(origin + library.path);
		await expectCatalogue(page, [first]);
		await refresh.focus();
		await expect(refresh).toBeFocused();
		const statusesBeforeRefresh = await main.getByRole('status').elementHandles();
		await page.keyboard.press('Enter');
		await expect(loading).toBeVisible();
		await expect(refresh).toBeFocused();
		const liveStatus = await loading.elementHandle();
		expect(
			await liveStatus.evaluate((node, previous) => previous.includes(node), statusesBeforeRefresh),
			'the loading status was already mounted before Refresh'
		).toBe(true);
		failed.resolve(503);
		await expect(retry).toBeFocused();
		await expect(stale).toHaveText(/^stale — updated /);
		expect(await stale.evaluate((node, previous) => node === previous, liveStatus)).toBe(true);

		await page.keyboard.press('Space');
		await expect(loading).toBeVisible();
		await expect(retry).toBeFocused();
		recovered.resolve([added]);
		await expectCatalogue(page, [added]);
		await expect(refresh).toBeFocused();
		await expect(stale).toHaveCount(0);
		await expect(refreshed).toHaveCount(1);
		expect(await refreshed.evaluate((node, previous) => node === previous, liveStatus)).toBe(true);

		await page.keyboard.press('Tab');
		await expect(main.getByRole('combobox', { name: 'Repeat', exact: true })).toBeFocused();
		await page.keyboard.press('Shift+Tab');
		await expect(refresh).toBeFocused();
		expect(fixture.listingAccounts).toEqual([listener.account, listener.account, listener.account]);
	} finally {
		failed.resolve(503);
		recovered.resolve([added]);
	}
});

test('returning to Library shows the new track and keeps the current selection', async ({ page, origin }) => {
	let catalogue = [first, selected];
	const fixture = await mockCatalogue(page, origin, () => catalogue);
	await fixture.signIn();
	await page.goto(origin + library.path);
	await expectCatalogue(page, catalogue);
	for (const track of catalogue) await row(page, track).locator(library.select).click();
	await page.getByTestId(player.queueItem)
		.getByRole('button', { name: `Select ${selected.title}`, exact: true }).click();
	const current = page.locator(player.currentQueueItem);
	await expect(current).toHaveAttribute(player.queueId, selected.id);
	const documentTimeOrigin = await page.evaluate(() => performance.timeOrigin);

	await page.getByRole('button', { name: 'Account menu', exact: true }).click();
	await page.getByRole('menuitem', { name: 'Settings', exact: true }).click();
	await expect(page.getByRole('heading', { name: 'Settings', exact: true })).toBeVisible();
	await expectCatalogue(page, []);
	catalogue = [added, first, selected];

	const libraryLink = page.getByRole('navigation', { name: 'Primary', exact: true })
		.getByRole('link', { name: 'Your Library', exact: true });
	const [response] = await Promise.all([page.waitForResponse(origin + library.api), libraryLink.click()]);
	expect(response.status()).toBe(200);
	await expect(page).toHaveURL(origin + library.path);
	await expectCatalogue(page, catalogue);
	await expect(current).toHaveAttribute(player.queueId, selected.id);
	expect(await page.evaluate(() => performance.timeOrigin)).toBe(documentTimeOrigin);
});

test('Refresh adds the first available track to an empty invited Library', async ({ page, origin }) => {
	const updated = { ...first, title: 'Updated catalogue track' };
	const initial = Promise.withResolvers<Track[]>();
	const firstRefresh = Promise.withResolvers<Track[]>();
	const repeatRefresh = Promise.withResolvers<Track[]>();
	let catalogue = initial.promise;
	const fixture = await mockCatalogue(page, origin, () => catalogue);
	await fixture.signIn();
	const loading = page.getByRole('status').filter({ hasText: /^Loading your library…$/ });
	const empty = page.getByRole('status').filter({ hasText: /^Your library is empty\.$/ });
	const refresh = page.getByRole('button', { name: library.refresh, exact: true });

	try {
		await page.goto(origin + library.path);
		await expect.poll(() => fixture.listingAccounts.length).toBe(1);
		await expect(loading).toBeVisible();
		await expect(empty).toBeHidden();
		await expectCatalogue(page, []);
		initial.resolve([]);
		await expect(empty).toBeVisible();
		await expect(loading).toBeHidden();
		const documentTimeOrigin = await page.evaluate(() => performance.timeOrigin);

		catalogue = firstRefresh.promise;
		expect(fixture.listingAccounts).toEqual([listener.account]);
		await refresh.click();
		await expect.poll(() => fixture.listingAccounts.length).toBe(2);
		await expect(loading).toBeVisible();
		await expect(empty).toBeHidden();
		await expectCatalogue(page, []);
		firstRefresh.resolve([first]);
		await expectCatalogue(page, [first]);
		await expect(empty).toBeHidden();
		await expect(loading).toBeHidden();

		catalogue = repeatRefresh.promise;
		await refresh.click();
		await expect.poll(() => fixture.listingAccounts.length).toBe(3);
		await expect(loading).toBeVisible();
		repeatRefresh.resolve([updated]);
		await expect(loading).toBeHidden();
		await expectCatalogue(page, [updated]);
		expect(fixture.listingAccounts).toEqual([listener.account, listener.account, listener.account]);
		await expect(page).toHaveURL(origin + library.path);
		expect(await page.evaluate(() => performance.timeOrigin)).toBe(documentTimeOrigin);
	} finally {
		initial.resolve([]);
		firstRefresh.resolve([first]);
		repeatRefresh.resolve([updated]);
	}
});

test('Title Z–A survives a failed invited Library refresh and successful Retry', async ({ page, origin }) => {
	const initial = [first, selected, added];
	const renamed = { ...first, title: 'Zulu catalogue track' };
	const newcomer = { id: '00000000-0000-4000-8000-000000000004', title: 'Amber catalogue track' };
	const refreshed = [renamed, newcomer, selected];
	let listing: Listing = initial;
	const fixture = await mockCatalogue(page, origin, () => listing);
	await fixture.signIn();

	const main = page.getByRole('main');
	const order = main.getByRole('combobox', { name: 'Order tracks', exact: true });
	const selectedOrder = order.locator('option:checked');
	const titles = main.getByTestId('library-list').getByTestId(library.item).locator(library.select);
	const stale = main.getByTestId('library-stale-badge');
	const refresh = main.getByRole('button', { name: library.refresh, exact: true });
	const retry = main.getByRole('button', { name: 'Retry', exact: true });
	const listingUrl = origin + library.api;
	const mode = 'Title Z–A';
	const initialSorted = [selected.title, added.title, first.title];

	await page.goto(origin + library.path);
	await expect(selectedOrder).toHaveText('Original order');
	await expect(titles).toHaveText(initial.map(({ title }) => title));
	await order.selectOption({ label: mode });
	await expect(selectedOrder).toHaveText(mode);
	await expect(titles).toHaveText(initialSorted);
	expect(fixture.listingAccounts).toEqual([listener.account]);

	listing = 503;
	const [failed] = await Promise.all([page.waitForResponse(listingUrl), refresh.click()]);
	expect(failed.status()).toBe(503);
	await expect(stale).toBeVisible();
	await expect(selectedOrder).toHaveText(mode);
	await expect(titles).toHaveText(initialSorted);
	expect(fixture.listingAccounts).toEqual([listener.account, listener.account]);

	listing = refreshed;
	const [retried] = await Promise.all([page.waitForResponse(listingUrl), retry.click()]);
	expect(retried.status()).toBe(200);
	await expect(stale).toBeHidden();
	await expect(refresh).toBeVisible();
	await expect(selectedOrder).toHaveText(mode);
	await expect(titles).toHaveText([renamed.title, selected.title, newcomer.title]);
	expect(fixture.listingAccounts).toEqual([listener.account, listener.account, listener.account]);
});
