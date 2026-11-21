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
	await page.getByLabel('Search your library').fill('Charlie');
	await page.getByRole('search').getByRole('button', { name: 'Search' }).click();
	await expect(page.getByRole('main').getByRole('listitem')).toHaveText('Charlie — Example Artist');
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

test('Home shows loading and recovers from a transient catalogue failure', async ({ page }) => {
	const shelf = discovery(page);
	await page.route('**/api/library', (route) => route.fulfill({ json: [rows[0]] }));
	await page.route(
		'**/api/library',
		async (route) => {
			await expect(shelf.getByRole('status')).toHaveText('Loading your library…');
			await expect(shelf.getByRole('listitem')).toHaveCount(0);
			await expect(shelf.getByRole('link', { name: 'Search your library' })).toHaveCount(0);
			await route.fulfill({ status: 503, body: '' });
		},
		{ times: 1 }
	);
	await page.goto('/home');

	await expect(shelf.getByRole('alert')).toContainText('Could not load your library');
	await expect(shelf.getByRole('listitem')).toHaveCount(0);
	await expect(shelf.getByRole('link', { name: 'Search your library' })).toHaveCount(0);
	await shelf.getByRole('button', { name: 'Retry' }).click();
	await expect(shelf.getByRole('listitem')).toHaveText('Zulu — Example Artist');
	await expect(shelf.getByRole('alert')).toHaveCount(0);
});
