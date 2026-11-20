import { expect, test } from '@playwright/test';

const plays = [
	{ play_id: 'Play_B', track_id: 'Job_A', played_at: 1_700_000_002, stream_seconds: 10 },
	{ play_id: 'Play_A', track_id: 'Job_A', played_at: 1_700_000_000, stream_seconds: 4 },
	{ play_id: 'Play_C', track_id: 'Job_Missing', played_at: 1_699_999_999, stream_seconds: 0 }
];

test('Recent lists only metered plays and clears them on an auth transition', async ({ page }) => {
	await page.route('**/v1/me/history', (route) => route.fulfill({ json: { items: plays } }));
	await page.route('**/v1/library', (route) => route.fulfill({ json: [
		{ id: 'recording', occurrence_key: 'Job_A:0', title: 'Coral Horizon' },
		{ id: 'never_played', title: 'Unplayed Title' }
	] }));
	await page.goto('/library/recent');

	const history = page.getByRole('region', { name: 'Recently played' });
	const entries = history.getByRole('listitem');
	await expect(entries).toHaveText([/Coral Horizon/, /Coral Horizon/, /Unknown track/]);
	await expect(entries.first().locator('time')).toHaveAttribute('datetime', '2023-11-14T22:13:22.000Z');
	await expect(page.getByRole('region', { name: 'Recent searches' })).toBeVisible();

	await page.evaluate(() => window.dispatchEvent(new StorageEvent('storage', {
		key: 'nntune.library.auth-transition.v1'
	})));
	await expect(history.getByRole('link', { name: 'Sign in' })).toBeVisible();
	await expect(entries).toHaveCount(0);
});

test('Recent offers retry after a read failure and distinguishes an empty result', async ({ page }) => {
	let requests = 0;
	await page.route('**/v1/me/history', (route) => ++requests === 1
		? route.fulfill({ status: 503, json: {} })
		: route.fulfill({ json: { items: [] } }));
	await page.goto('/library/recent');

	const history = page.getByRole('region', { name: 'Recently played' });
	await expect(history.getByRole('alert')).toContainText('Could not load');
	await history.getByRole('button', { name: 'Retry' }).click();
	await expect(history.getByText('No recent plays yet.')).toBeVisible();
	await expect(history.getByRole('listitem')).toHaveCount(0);
});
