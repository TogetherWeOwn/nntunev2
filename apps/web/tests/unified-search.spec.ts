import { expect, test, type Page } from '@playwright/test';

const artistId = '11111111-1111-1111-1111-111111111111';
const albumId = '22222222-2222-2222-2222-222222222222';
const recordingId = '33333333-3333-3333-3333-333333333333';
const missingId = '44444444-4444-4444-4444-444444444444';

async function fixtures(page: Page) {
	await page.route('**/api/library', (route) => route.fulfill({ json: [
		{ id: 'coral', title: 'Coral Song', artist: 'Reef Band', recording_mbids: [recordingId], artist_mbid: artistId, release_mbid: albumId }
	] }));
	await page.route('**/v1/mb/search?**', (route) => {
		const url = new URL(route.request().url());
		const kind = url.searchParams.get('type');
		const results = kind === 'artist' ? [{ mbid: artistId, name: 'Coral Artist', score: 0.9 }]
			: kind === 'release' ? [{ mbid: albumId, title: 'Coral Album', artist_name: 'Reef Band', score: 0.8 }]
			: [{ mbid: recordingId, title: 'Coral Song', score: 1 }, { mbid: missingId, title: 'Unindexed Recording', score: 0.7 }];
		return route.fulfill({ json: { q: url.searchParams.get('q'), type: kind, limit: 12, offset: 0, results } });
	});
}

async function hydrated(page: Page) {
	await expect(page.locator('html')).toHaveAttribute('data-hydrated', '');
}

test('unifies library and catalogue matches, hides unindexed recordings and uses entity links', async ({ page }) => {
	await fixtures(page);
	await page.goto('/search?q=Coral');
	await hydrated(page);
	const main = page.locator('main');
	await expect(main.getByRole('link', { name: 'Coral Artist', exact: true })).toHaveAttribute('href', `/artist/${artistId}`);
	await expect(main.getByRole('link', { name: 'Coral Album — Reef Band', exact: true })).toHaveAttribute('href', `/album/${albumId}`);
	await expect(main.getByRole('link', { name: 'Coral Song — Reef Band', exact: true })).toHaveAttribute('href', '/library?track=coral&t=0');
	await expect(main.getByText('Unindexed Recording')).toHaveCount(0);
	await main.getByRole('link', { name: 'Albums', exact: true }).click();
	await expect(page).toHaveURL(/\/search\/Coral\/albums$/);
	await expect(main.getByRole('link', { name: 'Albums', exact: true })).toHaveAttribute('aria-current', 'page');
	await expect(main.getByRole('link', { name: 'Coral Song — Reef Band', exact: true })).toHaveCount(0);
	await main.getByRole('link', { name: 'Playlists', exact: true }).click();
	await expect(main.getByText('No playlists found for “Coral”.', { exact: false })).toBeVisible();
	await expect(main.getByRole('link', { name: 'Clear search', exact: true })).toHaveAttribute('href', '/search');
});

test('keeps library matches usable when catalogue fails and exposes a retry', async ({ page }) => {
	await fixtures(page);
	await page.route('**/v1/mb/search?**', (route) => route.fulfill({ status: 503, json: {} }));
	await page.goto('/search?q=Coral');
	await expect(page.getByText('Catalogue search is unavailable. Showing library matches only.')).toBeVisible();
	await expect(page.getByRole('button', { name: 'Retry catalogue', exact: true })).toBeVisible();
	await expect(page.locator('main').getByRole('link', { name: 'Coral Song — Reef Band', exact: true })).toBeVisible();
});

test('settles catalogue loading after a library access refusal', async ({ page }) => {
	await fixtures(page);
	await page.route('**/api/library', (route) => route.fulfill({ status: 403, json: {} }));
	await page.route('**/v1/mb/search?**', (route) => route.abort());
	await page.goto('/search?q=Coral');
	await expect(page.getByText('Your library is not available. Catalogue results may be incomplete.')).toBeVisible();
	await expect(page.getByText('Searching the catalogue…', { exact: true })).toHaveCount(0);
	await expect(page.getByText('No music found for “Coral”.', { exact: false })).toBeVisible();
});

test('records explicit searches locally with remove and clear controls, then clears at an auth boundary', async ({ page }) => {
	await fixtures(page);
	await page.goto('/search');
	await hydrated(page);
	await expect(page.getByText('Loading your library…', { exact: true })).toHaveCount(0);
	const field = page.locator('main').getByRole('searchbox', { name: 'Search music', exact: true });
	await field.fill('Coral');
	await page.locator('main').getByRole('button', { name: 'Search', exact: true }).click();
	await expect(page).toHaveURL(/\/search\/Coral\/all$/);
	await page.goto('/search');
	await expect(page.getByRole('region', { name: 'Recent searches' }).getByRole('link', { name: 'Coral', exact: true })).toBeVisible();
	await page.getByRole('button', { name: 'Remove Coral from recent searches', exact: true }).click();
	await expect(page.getByText('Your searches will appear here.')).toBeVisible();
	await page.evaluate(() => window.dispatchEvent(new StorageEvent('storage', { key: 'nntune.library.auth-transition.v1', newValue: 'next-account' })));
	await expect(page.getByText('Sign in to search your library.', { exact: false })).toBeVisible();
	await expect(page.getByRole('region', { name: 'Recent searches' })).toHaveCount(0);
});

test('opens quick search on demand, supports keyboard hits, and restores focus on Escape', async ({ page }) => {
	await fixtures(page);
	await page.goto('/settings');
	await hydrated(page);
	const opener = page.locator('[data-testid="shell-topbar"]').getByRole('searchbox', { name: 'Search', exact: true });
	await opener.focus();
	await page.keyboard.press('Control+k');
	const dialog = page.getByRole('dialog', { name: 'Quick search', exact: true });
	await expect(dialog).toBeVisible();
	const input = dialog.getByRole('searchbox', { name: 'Search music', exact: true });
	await input.fill('Coral');
	await expect(dialog.getByRole('link', { name: 'Coral Artist', exact: true })).toBeVisible();
	await input.press('ArrowDown');
	await expect(dialog.locator('[data-search-result]').first()).toBeFocused();
	await page.keyboard.press('Escape');
	await expect(dialog).toHaveCount(0);
	await expect(opener).toBeFocused();
});

test('does not let a pending shell search override a different destination', async ({ page }) => {
	await fixtures(page);
	await page.goto('/settings');
	await hydrated(page);
	await page.clock.install();
	await page.clock.pauseAt(new Date());
	await page.locator('[data-testid="shell-topbar"]').getByRole('searchbox', { name: 'Search', exact: true }).fill('Coral');
	await page.locator('[data-testid="shell-rail"]').getByRole('link', { name: 'Home', exact: true }).click();
	await expect(page).toHaveURL(/\/home$/);
	await page.clock.runFor(500);
	await expect(page).toHaveURL(/\/home$/);
});

test('debounces shell typing while retaining keyboard focus', async ({ page }) => {
	await fixtures(page);
	await page.goto('/settings');
	await hydrated(page);
	const input = page.locator('[data-testid="shell-topbar"]').getByRole('searchbox', { name: 'Search', exact: true });
	await input.fill('Coral');
	await expect(page).toHaveURL(/\/search\?q=Coral$/);
	await expect(input).toBeFocused();
	await expect(page.locator('main').getByRole('link', { name: 'Coral Artist', exact: true })).toBeVisible();
});
