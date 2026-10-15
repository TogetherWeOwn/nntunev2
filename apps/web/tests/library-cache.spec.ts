import { expect, test } from '@playwright/test';

const SNAPSHOT_KEY = 'nntune.library.snapshot.v1';

const SEEDED = {
	savedAt: '2026-10-15T11:00:00.000Z',
	items: [
		{ id: 'track-seed-1', title: 'Seeded Song One', artist: 'Seed Artist' },
		{ id: 'track-seed-2', title: 'Seeded Song Two' }
	]
};

test('offline library renders the stale snapshot with a badge when the API fails', async ({
	page
}) => {
	// Seed the last-good snapshot before any app script runs, then take the
	// API down (tunnel down / Access hiccup / airplane wifi).
	await page.addInitScript(
		({ key, snapshot }) => {
			window.localStorage.setItem(key, JSON.stringify(snapshot));
		},
		{ key: SNAPSHOT_KEY, snapshot: SEEDED }
	);
	await page.route('**/api/library', (route) => route.abort('failed'));

	await page.goto('/library');

	const list = page.getByTestId('library-list');
	await expect(list, 'stale library list missing').toBeVisible();
	await expect(page.getByTestId('library-item')).toHaveCount(2);
	await expect(list).toContainText('Seeded Song One');

	const badge = page.getByTestId('library-stale-badge');
	await expect(badge, 'stale badge missing').toBeVisible();
	await expect(badge).toContainText(/stale — updated /i);
});

test('fresh library success replaces the snapshot and hides the badge', async ({ page }) => {
	const fresh = [{ id: 'track-fresh-1', title: 'Fresh Song' }];
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(fresh)
		})
	);

	await page.goto('/library');

	await expect(page.getByTestId('library-list'), 'fresh library list missing').toBeVisible();
	await expect(page.getByTestId('library-item')).toHaveCount(1);
	await expect(page.getByTestId('library-list')).toContainText('Fresh Song');
	await expect(page.getByTestId('library-stale-badge'), 'badge should hide on fresh').toHaveCount(
		0
	);

	const stored = await page.evaluate((key) => window.localStorage.getItem(key), SNAPSHOT_KEY);
	expect(stored, 'fresh fetch did not persist a snapshot').not.toBeNull();
	expect(JSON.parse(stored as string).items.map((i: { id: string }) => i.id)).toEqual([
		'track-fresh-1'
	]);
});
