import { expect, test, type Page } from '@playwright/test';

const releaseId = 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa';
const artistId = 'bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb';
const ITEMS = [
	{ id: 'linked', title: 'Linked Song', artist: 'Linked Artist', release_mbid: releaseId, artist_mbid: artistId },
	{ id: 'plain', title: 'Plain Song', artist: 'Plain Artist', release_mbid: 'nope', artist_mbid: '' }
];

async function seed(page: Page, url: string) {
	await page.route('**/api/library', (route) =>
		route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(ITEMS) }));
	await page.goto(url);
}

test('library rows link valid ids beside, not inside, the play button', async ({ page }) => {
	await seed(page, '/library');
	const linked = page.locator('[data-testid="library-item"][data-id="linked"]');
	await expect(linked).toBeVisible();
	await expect(linked.getByRole('link', { name: 'Album for Linked Song' })).toHaveAttribute('href', `/album/${releaseId}`);
	await expect(linked.getByRole('link', { name: 'Artist Linked Artist' })).toHaveAttribute('href', `/artist/${artistId}`);
	await expect(linked.locator('button a, a button')).toHaveCount(0);
	await expect(linked.locator('button.nt-library-row')).toHaveCount(1);
	const plain = page.locator('[data-testid="library-item"][data-id="plain"]');
	await expect(plain.getByRole('link')).toHaveCount(0);
	await expect(plain.locator('button.nt-library-row')).toContainText('Plain Song');
});

test('following a row link does not play or select the row', async ({ page }) => {
	let playbackRequests = 0;
	page.on('request', (request) => {
		if (/\/(?:streams|play-grants)(?:\/|\?|$)/.test(new URL(request.url()).pathname)) playbackRequests += 1;
	});
	await page.route('**/v1/**', (route) => route.fulfill({ status: 404, body: '' }));
	await seed(page, '/library');
	const linked = page.locator('[data-testid="library-item"][data-id="linked"]');
	await expect(linked).not.toHaveAttribute('data-selected');
	await page.getByRole('link', { name: 'Album for Linked Song' }).focus();
	await page.keyboard.press('Enter');
	await expect(page).toHaveURL(new RegExp(`/album/${releaseId}$`));
	expect(playbackRequests).toBe(0);
	await page.goBack();
	await expect(linked).toBeVisible();
	await expect(linked).not.toHaveAttribute('data-selected');
});

test('search results link valid ids and leave others as text', async ({ page }) => {
	await seed(page, '/search?q=song');
	const results = page.locator('ul li');
	await expect(results).toHaveCount(2);
	await expect(results.nth(0).getByRole('link', { name: 'Album for Linked Song' })).toHaveAttribute('href', `/album/${releaseId}`);
	await expect(results.nth(0).getByRole('link', { name: 'Linked Artist' })).toHaveAttribute('href', `/artist/${artistId}`);
	await expect(results.nth(1).getByRole('link')).toHaveCount(0);
	await expect(results.nth(1)).toHaveText('Plain Song — Plain Artist');
});
