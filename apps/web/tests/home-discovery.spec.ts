import { expect, test, type Page } from '@playwright/test';

const rows = ['Zulu', 'Echo', 'Alpha', 'Delta', 'Bravo', 'Charlie'].map((title, i) => ({
	id: `recording-${i}`,
	title,
	artist: 'Example Artist'
}));

function discovery(page: Page) {
	return page.getByRole('region', { name: 'Discover your library' });
}

test('Home previews up to five catalogue rows in order and links to Search', async ({ page }) => {
	await page.addInitScript(() => {
		const context = 'home-discovery-test';
		sessionStorage.setItem('nntune.library.auth-context.v1', context);
		localStorage.setItem(`nntune.library.favorites.v1.${context}`, JSON.stringify(['recording-5']));
		localStorage.setItem(`nntune.library.recent-finds.v1.${context}`, JSON.stringify(['Charlie']));
	});
	let catalogueRequests = 0;
	await page.route('**/api/library', (route) => {
		catalogueRequests++;
		return route.fulfill({ json: rows });
	});
	await page.goto('/home');

	const shelf = discovery(page);
	await expect(shelf).toContainText('the same library catalogue as Search');
	await expect(shelf).toContainText(
		'Your account listening history is not used to select or rank them.'
	);
	await expect(shelf.getByRole('heading', { name: 'From your library (6)' })).toBeVisible();
	await expect(
		page.getByRole('main').getByRole('heading', { name: 'From your library (6)', exact: true })
	).toHaveCount(1);
	await expect(shelf.getByRole('listitem')).toHaveText([
		'Zulu — Example Artist',
		'Echo — Example Artist',
		'Alpha — Example Artist',
		'Delta — Example Artist',
		'Bravo — Example Artist'
	]);
	await expect(shelf.getByRole('link', { name: 'Open Your Library' })).toHaveAttribute(
		'href',
		'/library'
	);
	await expect(page.getByRole('region', { name: 'Liked (1)' }).getByRole('link')).toHaveAttribute(
		'href',
		'/library/liked'
	);
	await expect(
		page.getByRole('region', { name: 'Recent searches' }).getByRole('link', { name: 'Charlie' })
	).toHaveAttribute('href', '/search?q=Charlie');
	expect(catalogueRequests).toBe(1);
	await shelf.getByRole('link', { name: 'Search your library' }).click();
	await expect(page).toHaveURL(/\/search$/);

	const main = page.getByRole('main');
	const topBarSearch = page
		.getByTestId('shell-topbar')
		.getByRole('search', { name: 'Top bar', exact: true });
	const librarySearch = main.getByRole('search', { name: 'Search your library', exact: true });
	await expect(page.getByRole('search')).toHaveCount(2);
	await expect(topBarSearch).toBeVisible();
	await expect(librarySearch).toBeVisible();

	await expect(main.getByText('Type above to search your library by title or artist.')).toBeVisible();
	const status = main.getByRole('status');
	await expect(status).toBeEmpty();
	const mountedStatus = await status.elementHandle();
	const field = librarySearch.getByRole('searchbox', { name: 'Search your library' });
	const results = main.getByRole('listitem');
	await field.fill('Charlie');
	await librarySearch.getByRole('button', { name: 'Search', exact: true }).click();
	await expect(page).toHaveURL('/search?q=Charlie');
	await expect(status).toHaveText('1 result for “Charlie”.');
	await expect(results).toHaveText('Charlie — Example Artist');
	expect(await status.evaluate((current, original) => current === original, mountedStatus)).toBe(true);

	await field.fill('No such track');
	await expect(page).toHaveURL('/search?q=Charlie');
	await expect(status).toHaveText('1 result for “Charlie”.');
	await field.press('Enter');
	await expect(status).toHaveText('No results for “No such track”.');
	await expect(results).toHaveCount(0);
	expect(await status.evaluate((current, original) => current === original, mountedStatus)).toBe(true);

	const topBarField = topBarSearch.getByRole('searchbox', { name: 'Search', exact: true });
	await topBarField.fill('Example Artist');
	await topBarField.press('Enter');
	await expect(field).toHaveValue('Example Artist');
	await expect(status).toHaveText('6 results for “Example Artist”.');
	await expect(results).toHaveCount(6);
	expect(await status.evaluate((current, original) => current === original, mountedStatus)).toBe(true);
});

for (const { status, message, action } of [
	{ status: 200, message: 'Your library is empty.', action: ['Open Your Library', '/library'] },
	{ status: 401, message: 'Sign in to see your library.', action: ['Sign in', '/login'] },
	{ status: 403, message: 'You do not have access to this library.' },
	{ status: 404, message: 'Your library is not available yet.' }
]) {
	test(`Home shows an honest discovery state for catalogue response ${status}`, async ({ page }) => {
		await page.route('**/api/library', (route) => route.fulfill({ status, json: [] }));
		await page.goto('/home');

		const shelf = discovery(page);
		await expect(shelf.getByText(message, { exact: false })).toBeVisible();
		await expect(shelf.getByRole('listitem')).toHaveCount(0);
		await expect(shelf.getByRole('link', { name: 'Search your library' })).toHaveCount(0);
		await expect(shelf.getByRole('button', { name: 'Retry' })).toHaveCount(0);
		if (action) {
			await expect(shelf.getByRole('link', { name: action[0] })).toHaveAttribute('href', action[1]);
		} else {
			await expect(shelf.getByRole('link')).toHaveCount(0);
		}
	});
}

test('Home keeps keyboard focus in the discovery shelf after Retry returns an empty library', async ({
	page
}) => {
	await page.route('**/api/library', (route) => route.fulfill({ json: [] }));
	await page.route('**/api/library', (route) => route.fulfill({ status: 503 }), { times: 1 });
	await page.goto('/home');

	const shelf = discovery(page);
	const retry = shelf.getByRole('button', { name: 'Retry' });
	await retry.focus();
	await page.keyboard.press('Enter');

	await expect(shelf.getByText('Your library is empty.', { exact: false })).toBeVisible();
	await expect(shelf.getByRole('heading', { name: 'Discover your library' })).toBeFocused();
	await page.keyboard.press('Tab');
	await expect(shelf.getByRole('link', { name: 'Open Your Library' })).toBeFocused();
});

test('Home keeps scoped recent searches hidden until a successful Retry', async ({ page }) => {
	const context = 'home-discovery-retry';
	const key = `nntune.library.recent-finds.v1.${context}`;
	const recent = page.getByRole('region', { name: 'Recent searches' });
	await page.addInitScript(({ context, key }) => {
		sessionStorage.setItem('nntune.library.auth-context.v1', context);
		localStorage.setItem(key, JSON.stringify(['Zulu & Echo']));
	}, { context, key });
	const deliverHistoryChange = (query: string) => page.evaluate(({ key, query }) => {
		localStorage.setItem(key, JSON.stringify([query]));
		window.dispatchEvent(new StorageEvent('storage', { key, storageArea: localStorage }));
	}, { key, query });
	const shelf = discovery(page);
	await page.route('**/api/library', (route) => route.fulfill({ json: [rows[0]] }));
	await page.route(
		'**/api/library',
		async (route) => {
			await expect(shelf.getByRole('status')).toHaveText('Loading your library…');
			await expect(shelf.getByRole('listitem')).toHaveCount(0);
			await expect(shelf.getByRole('link', { name: 'Search your library' })).toHaveCount(0);
			await expect(recent).toHaveCount(0);
			await deliverHistoryChange('Pending query');
			await expect(recent).toHaveCount(0);
			await route.fulfill({ status: 503, body: '' });
		},
		{ times: 1 }
	);
	await page.goto('/home');

	await expect(shelf.getByRole('alert')).toContainText('Could not load your library');
	await expect(shelf.getByRole('listitem')).toHaveCount(0);
	await expect(shelf.getByRole('link', { name: 'Search your library' })).toHaveCount(0);
	await expect(recent).toHaveCount(0);
	await deliverHistoryChange('Retry & match');
	await expect(recent).toHaveCount(0);

	await shelf.getByRole('button', { name: 'Retry' }).click();
	await expect(shelf.getByRole('listitem')).toHaveText('Zulu — Example Artist');
	await expect(shelf.getByRole('alert')).toHaveCount(0);
	await expect(recent.getByRole('link', { name: 'Retry & match' })).toHaveAttribute(
		'href', '/search?q=Retry%20%26%20match'
	);
	await deliverHistoryChange('Updated query');
	await expect(recent.getByRole('link')).toHaveText('Updated query');
});
