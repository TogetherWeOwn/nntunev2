import { expect, test, type Page } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';

const albumId = 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa';
const artistId = 'bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb';
const trackId = 'cccccccc-cccc-cccc-cccc-cccccccccccc';
const editionId = 'eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee';
const groupId = 'dddddddd-dddd-dddd-dddd-dddddddddddd';
const release = { mbid: albumId, title: 'Quiet Rooms', release_type: 'Album', year: 2024,
	artist_mbid: artistId, artist: 'North Window', art_url: `/v1/releases/${albumId}/art`, release_group_mbid: groupId };
const edition = { ...release, mbid: editionId, year: 2025, art_url: `/v1/releases/${editionId}/art` };
const artist = { mbid: artistId, name: 'North Window', sort_name: 'North Window', type: 'Group', country: null };
const track = { mbid: trackId, title: 'First Light', release_mbid: albumId, release: release.title,
	artist_mbid: artistId, artist: release.artist, duration_ms: 90_000, track_no: 1, medium_no: 1, art_url: release.art_url };

async function catalogue(page: Page) {
	await page.route('**/v1/releases/*/art', (route) => route.fulfill({ status: 404, body: '' }));
	await page.route('**/v1/releases/*', (route) => {
		const id = new URL(route.request().url()).pathname.split('/').pop();
		return route.fulfill({ contentType: 'application/json', body: JSON.stringify({
			release: id === editionId ? edition : release,
			tracks: id === editionId ? [] : [track, { ...track, medium_no: 2, duration_ms: null }]
		}) });
	});
	await page.route('**/v1/artists/*', (route) => route.fulfill({ contentType: 'application/json', body: JSON.stringify({ artist, releases: [release, edition], truncated: false }) }));
}

test('album preserves multi-disc occurrences, unknown durations and real entity destinations', async ({ page }) => {
	await catalogue(page);
	await page.goto(`/album/${albumId}`);
	await expect(page.getByRole('heading', { name: 'Quiet Rooms', level: 1 })).toBeVisible();
	await expect(page.getByText('2 available tracks · Total duration unknown')).toBeVisible();
	await expect(page.getByRole('table', { name: 'Disc 1' })).toBeVisible();
	await expect(page.getByRole('table', { name: 'Disc 2' })).toBeVisible();
	await expect(page.getByRole('cell', { name: 'Unknown', exact: true })).toBeVisible();
	await expect(page.getByRole('link', { name: 'Open First Light in library' })).toHaveCount(2);
	await expect(page.getByRole('link', { name: 'Open First Light in library' }).first()).toHaveAttribute('href', `/library?track=${trackId}&t=0`);
	await expect(page.getByRole('heading', { name: 'Other available editions' })).toBeVisible();
	await expect(page.getByRole('img', { name: 'Cover for Quiet Rooms' }).first()).toHaveAttribute('data-fallback', 'true');
	await page.getByRole('link', { name: 'North Window', exact: true }).click();
	await expect(page).toHaveURL(`/artist/${artistId}`);
	await expect(page.getByRole('heading', { name: 'North Window', level: 1 })).toBeVisible();
	await expect(page.getByRole('heading', { name: 'Appears on' })).toBeVisible();
	await expect(page.getByText('Guest appearances are not available in this mirror response.')).toBeVisible();
	await expect(page.getByRole('button', { name: /follow|save/i })).toHaveCount(0);
});

test('artist groups editions, filters release types and resets filters on a new artist', async ({ page }) => {
	await catalogue(page);
	await page.goto(`/artist/${artistId}`);
	await expect(page.locator('.catalogue-grid > li')).toHaveCount(1);
	await page.getByText('2 available editions', { exact: true }).click();
	await expect(page.getByRole('link', { name: /Quiet Rooms · 2025/ })).toHaveAttribute('href', `/album/${editionId}`);
	await page.getByRole('button', { name: 'Compilations', exact: true }).click();
	await expect(page.getByRole('article').getByRole('status')).toHaveText('No compilations available in this selection.');
	await expect(page.getByRole('button', { name: 'Compilations', exact: true })).toHaveAttribute('aria-pressed', 'true');
	await page.getByRole('button', { name: 'All', exact: true }).click();
	await page.locator('.catalogue-card').click();
	await expect(page.getByRole('heading', { name: 'Quiet Rooms', level: 1 })).toBeVisible();
	await page.getByRole('link', { name: 'North Window', exact: true }).click();
	await expect(page.getByRole('button', { name: 'All', exact: true })).toHaveAttribute('aria-pressed', 'true');
});

for (const entity of ['album', 'artist']) {
	test(`${entity} keeps signed-out, absent and retryable failures distinct`, async ({ page }) => {
		let status = 401;
		await page.route('**/v1/**', (route) => route.fulfill({ status, contentType: 'application/json', body: JSON.stringify({ code: 'unavailable', message: 'diagnostic', retryable: false }) }));
		const url = `/${entity}/${entity === 'album' ? albumId : artistId}`;
		await page.goto(url);
		await expect(page.getByRole('link', { name: 'Sign in', exact: true })).toHaveAttribute('href', '/login');
		status = 404;
		await page.reload();
		await expect(page.getByText(`This ${entity === 'album' ? 'release' : 'artist'} is not available in your catalogue.`)).toBeVisible();
		await expect(page.getByRole('button', { name: 'Retry' })).toHaveCount(0);
		status = 503;
		await page.reload();
		await expect(page.getByRole('button', { name: 'Retry' })).toBeVisible();
		await catalogue(page);
		await page.getByRole('button', { name: 'Retry' }).click();
		await expect(page.getByRole('heading', { level: 1, name: entity === 'album' ? 'Quiet Rooms' : 'North Window' })).toBeVisible();
	});
}

for (const width of [320, 1280]) {
	test(`catalogue pages remain keyboard accessible without overflow at ${width}px`, async ({ page }) => {
		await page.setViewportSize({ width, height: 900 });
		await catalogue(page);
		for (const url of [`/album/${albumId}`, `/artist/${artistId}`]) {
			await page.goto(url);
			await expect(page.getByRole('heading', { level: 1, name: url.startsWith('/album') ? 'Quiet Rooms' : 'North Window' })).toBeVisible();
			const action = url.startsWith('/album') ? page.getByRole('link', { name: 'North Window', exact: true }) : page.getByRole('button', { name: 'Albums', exact: true });
			await action.focus();
			await expect(action).toBeFocused();
			if (url.startsWith('/artist')) {
				await page.keyboard.press('Enter');
				await expect(action).toHaveAttribute('aria-pressed', 'true');
			}
			const results = await new AxeBuilder({ page }).include('.catalogue-page').withTags(['wcag2a', 'wcag2aa', 'wcag21aa']).analyze();
			expect(results.violations.filter((item) => item.impact === 'serious' || item.impact === 'critical')).toEqual([]);
			expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
			await page.screenshot({ path: test.info().outputPath(`${url.startsWith('/album') ? 'album' : 'artist'}-${width}.png`), fullPage: true });
		}
	});
}

test('a capped artist discography is labelled incomplete', async ({ page }) => {
	await catalogue(page);
	await page.route('**/v1/artists/*', (route) => route.fulfill({ contentType: 'application/json', body: JSON.stringify({ artist, releases: [release], truncated: true }) }));
	await page.goto(`/artist/${artistId}`);
	await expect(page.getByRole('article').getByRole('status')).toContainText('This discography may be incomplete.');
});
