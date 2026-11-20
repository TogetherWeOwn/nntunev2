import { expect, test } from '@playwright/test';

const id = '11111111-1111-4111-8111-111111111111';
const artPath = `/v1/releases/${id}/art`;
const albums = [
	{ mbid: id, title: 'Zulu Album', artist: 'North', art_url: artPath },
	{ mbid: '22222222-2222-4222-8222-222222222222', title: 'Aria', artist: 'South' }
];
const tracks = [
	{ id: 'track-a', occurrence_key: 'job:1', title: 'Track One' },
	{ id: 'track-a', occurrence_key: 'job:2', title: 'Track Two' }
];

test('browses member collections without changing track selection or queue', async ({ page }) => {
	await page.setViewportSize({ width: 360, height: 900 });
	await page.route('**/api/library', (route) => route.fulfill({ json: tracks }));
	await page.route('**/v1/releases', (route) => route.fulfill({ json: albums }));
	await page.route('**/v1/artists', (route) => route.fulfill({ json: [{ mbid: id, name: 'North' }] }));
	await page.route(`**${artPath}`, (route) => route.fulfill({ status: 404 }));
	await page.goto('/library');
	const trackRows = page.getByTestId('library-item');
	await expect(trackRows).toHaveCount(2);
	await trackRows.first().getByRole('checkbox').check();
	await page.getByRole('button', { name: 'Append selected', exact: true }).click();
	const chips = page.getByRole('group', { name: 'Library type' });
	await chips.getByRole('button', { name: 'All collections' }).click();
	const region = page.getByRole('region', { name: 'Library collections' });
	const rows = region.getByRole('listitem');
	await expect(rows).toHaveText([/Zulu Album/, /Aria/, /North/]);
	await expect(trackRows.first()).toBeHidden();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	await expect(page.getByTestId('usage-meter')).toHaveCount(1);
	await chips.getByRole('button', { name: 'Albums', exact: true }).click();
	await region.getByRole('combobox', { name: 'Sort collections' }).selectOption('title');
	await expect(rows).toHaveText([/Aria/, /Zulu Album/]);
	await region.getByRole('searchbox').fill('north');
	await expect(rows).toHaveText([/Zulu Album/]);
	const comfortable = (await rows.first().boundingBox())!.height;
	await page.getByRole('combobox', { name: 'Density' }).selectOption('compact');
	await expect.poll(async () => (await rows.first().boundingBox())!.height).toBeLessThan(comfortable);
	await region.getByRole('combobox', { name: 'Collection view' }).selectOption('grid');
	await expect(region.getByRole('list')).toHaveCSS('display', 'grid');
	await expect(rows.locator('img')).toHaveAttribute('src', '/placeholder-artwork.svg');
	await chips.getByRole('button', { name: 'Tracks', exact: true }).click();
	await expect(trackRows.first().getByRole('checkbox')).toBeChecked();
});

test('clears collections and the find query when the account changes', async ({ page }) => {
	await page.route('**/api/library', (route) => route.fulfill({ json: [] }));
	let requests = 0;
	await page.route('**/v1/releases', (route) => ++requests === 1
		? route.fulfill({ status: 503, json: {} }) : route.fulfill({ json: albums }));
	await page.goto('/library');
	await page.getByRole('group', { name: 'Library type' }).getByRole('button', { name: 'Albums' }).click();
	const region = page.getByRole('region', { name: 'Library collections' });
	await expect(region.getByRole('alert')).toContainText('Could not load');
	await region.getByRole('button', { name: 'Retry collections' }).click();
	await region.getByRole('searchbox').fill('North');
	await expect(region.getByRole('listitem')).toHaveText([/Zulu Album/]);
	await page.evaluate(() => window.dispatchEvent(new StorageEvent('storage', {
		key: 'nntune.library.auth-transition.v1'
	})));
	await expect(region.getByRole('link', { name: 'Sign in' })).toBeVisible();
	await expect(region.getByRole('searchbox')).toHaveValue('');
	await expect(region.getByRole('listitem')).toHaveCount(0);
});
