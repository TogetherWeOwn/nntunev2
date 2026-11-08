import { expect, test, type Page } from '@playwright/test';

const A = '11111111-1111-4111-8111-111111111111';
const B = '22222222-2222-4222-8222-222222222222';
const ALBUMS = [
	{ mbid: A, title: 'Zulu Album', artist: 'North', year: 2024, art_url: `/v1/releases/${A}/art` },
	{ mbid: B, title: 'Azure Album', artist: 'South', year: null, art_url: 'https://unapproved.invalid/art' }
];
const TRACKS = [
	{ id: 'track-a', occurrence_key: 'job:1', title: 'Track One', artist: 'North' },
	{ id: 'track-a', occurrence_key: 'job:2', title: 'Track One', artist: 'North' }
];

async function setup(page: Page): Promise<void> {
	await page.route('**/api/library', (route) => route.fulfill({ json: TRACKS }));
	await page.route('**/v1/releases', (route) => route.fulfill({ json: ALBUMS }));
	await page.route('**/v1/artists', (route) => route.fulfill({ json: [{ mbid: A, name: 'North', sort_name: 'North' }] }));
	await page.route(`**/v1/releases/${A}/art`, (route) => route.fulfill({ status: 404 }));
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(2);
}

function chips(page: Page) { return page.getByRole('group', { name: 'Library type' }); }
function collections(page: Page) { return page.getByRole('region', { name: 'Library collections' }); }

test('keeps occurrence rows, selection and singleton meter across collection browsing', async ({ page }) => {
	await setup(page);
	await page.getByTestId('library-item').first().getByRole('checkbox').check();
	await chips(page).getByRole('button', { name: 'Albums', exact: true }).click();
	await expect(collections(page).getByTestId('library-collection')).toHaveCount(2);
	await expect(page.getByTestId('library-item').first()).toBeHidden();
	await expect(page.getByTestId('usage-meter')).toHaveCount(1);
	await chips(page).getByRole('button', { name: 'Tracks', exact: true }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(2);
	await expect(page.getByTestId('library-item').first().getByRole('checkbox')).toBeChecked();
});

for (const width of [1280, 390, 360]) {
	test(`server-backed list/grid, find, sort and art fallback at ${width}px`, async ({ page }) => {
		await page.setViewportSize({ width, height: 900 });
		await setup(page);
		await chips(page).getByRole('button', { name: 'Albums', exact: true }).click();
		const region = collections(page);
		const rows = region.getByTestId('library-collection');
		await expect(rows).toHaveCount(2);
		await region.getByRole('combobox', { name: 'Sort collections' }).selectOption('title');
		await expect(rows.first()).toContainText('Azure Album');
		await region.getByRole('combobox', { name: 'Collection view' }).selectOption('grid');
		await expect(region.locator('[data-view="grid"]')).toBeVisible();
		await page.getByRole('combobox', { name: 'Density', exact: true }).selectOption('compact');
		await expect(region).toHaveAttribute('data-density', 'compact');
		await expect(rows.first().getByTestId('track-artwork')).toHaveAttribute('src', '/placeholder-artwork.svg');
		await expect(rows.last().getByTestId('track-artwork')).toHaveAttribute('src', '/placeholder-artwork.svg');
		await region.getByRole('searchbox', { name: 'Search in Your Library' }).fill('North');
		await expect(rows).toHaveCount(1);
		await expect(rows.first()).toContainText('Zulu Album');
		expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
	});
}

test('All reads both entity types without conflating shared MBIDs', async ({ page }) => {
	await setup(page);
	await chips(page).getByRole('button', { name: 'All', exact: true }).click();
	await expect(collections(page).getByTestId('library-collection')).toHaveCount(3);
	await expect(collections(page).getByRole('link', { name: 'Liked Songs' })).toHaveAttribute('href', '/library/liked');
});

test('refusal clears rendered collections and offers explicit retry, not stale private content', async ({ page }) => {
	await setup(page);
	await page.route('**/v1/releases', (route) => route.fulfill({ status: 401, json: { code: 'unauthorized' } }));
	await chips(page).getByRole('button', { name: 'Albums', exact: true }).click();
	await expect(collections(page).getByRole('link', { name: 'Sign in' })).toBeVisible();
	await expect(collections(page).getByTestId('library-collection')).toHaveCount(0);
	await page.unroute('**/v1/releases');
	await page.route('**/v1/releases', (route) => route.fulfill({ json: [] }));
	await collections(page).getByRole('button', { name: 'Retry collections' }).click();
	await expect(collections(page)).toContainText('No albums in your library yet.');
});

test('auth-boundary events fence a late collection response and clear the find query', async ({ page }) => {
	await setup(page);
	let requested!: () => void;
	const started = new Promise<void>((resolve) => { requested = resolve; });
	let release!: () => void;
	const response = new Promise<void>((resolve) => { release = resolve; });
	await page.route('**/v1/releases', async (route) => {
		requested();
		await response;
		await route.fulfill({ json: ALBUMS }).catch(() => {});
	});
	await chips(page).getByRole('button', { name: 'Albums', exact: true }).click();
	await started;
	await collections(page).getByRole('searchbox').fill('Old account query');
	await page.evaluate(() => window.dispatchEvent(new StorageEvent('storage', { key: 'nntune.library.auth-transition.v1' })));
	release();
	await expect(collections(page).getByRole('searchbox')).toHaveValue('');
	await expect(collections(page).getByTestId('library-collection')).toHaveCount(0);
	await expect(collections(page).getByRole('alert')).toContainText('Your session changed');
});
