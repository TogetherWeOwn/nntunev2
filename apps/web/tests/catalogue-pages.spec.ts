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
	await page.route('**/v1/me/library/albums/*', (route) => route.fulfill({ contentType: 'application/json', body: JSON.stringify({ saved: false, saved_at: null }) }));
	await page.route('**/v1/me/library/artists/*', (route) => route.fulfill({ contentType: 'application/json', body: JSON.stringify({ followed: false, followed_at: null }) }));
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
	await expect(page.getByRole('cell', { name: 'First Light', exact: true })).toHaveCount(2);
	// Each destination carries release and exact medium/position, not a first-page library lookup.
	await expect(page.getByRole('table', { name: 'Disc 1' }).getByRole('link', { name: 'First Light' }))
		.toHaveAttribute('href', `/track/${trackId}?release=${albumId}&medium=1&position=1`);
	await expect(page.getByRole('table', { name: 'Disc 2' }).getByRole('link', { name: 'First Light' }))
		.toHaveAttribute('href', `/track/${trackId}?release=${albumId}&medium=2&position=1`);
	await expect(page.getByRole('heading', { name: 'Other available editions' })).toBeVisible();
	await expect(page.getByRole('img', { name: 'Cover for Quiet Rooms' }).first()).toHaveAttribute('data-fallback', 'true');
	await page.getByRole('link', { name: 'North Window', exact: true }).click();
	await expect(page).toHaveURL(`/artist/${artistId}`);
	await expect(page.getByRole('heading', { name: 'North Window', level: 1 })).toBeVisible();
	await expect(page.getByRole('heading', { name: 'Appears on' })).toBeVisible();
	await expect(page.getByText('Guest appearances are not available in this mirror response.')).toBeVisible();
	await expect(page.getByRole('button', { name: 'Follow', exact: true })).toBeEnabled();
	await expect(page.getByRole('button', { name: 'Follow', exact: true })).toHaveAttribute('aria-pressed', 'false');
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
		for (const [entity, url, title] of [
			['album', `/album/${albumId}`, 'Quiet Rooms'],
			['artist', `/artist/${artistId}`, 'North Window'],
			['track', `/track/${trackId}?release=${albumId}&medium=1&position=1`, 'First Light']
		]) {
			await page.goto(url);
			await expect(page.getByRole('heading', { level: 1, name: title })).toBeVisible();
			const action = entity === 'artist' ? page.getByRole('button', { name: 'Albums', exact: true }) : page.getByRole('link', { name: 'North Window', exact: true });
			await action.focus();
			await expect(action).toBeFocused();
			if (entity === 'artist') {
				await page.keyboard.press('Enter');
				await expect(action).toHaveAttribute('aria-pressed', 'true');
			}
			const results = await new AxeBuilder({ page }).include('.catalogue-page').withTags(['wcag2a', 'wcag2aa', 'wcag21aa']).analyze();
			expect(results.violations.filter((item) => item.impact === 'serious' || item.impact === 'critical')).toEqual([]);
			expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
			await page.screenshot({ path: test.info().outputPath(`${entity}-${width}.png`), fullPage: true });
		}
	});
}

for (const entity of ['album', 'artist']) {
	test(`${entity} clears displayed member data after a delivered cross-tab auth transition`, async ({ page, context }) => {
		await catalogue(page);
		await page.goto(`/${entity}/${entity === 'album' ? albumId : artistId}`);
		await expect(page.getByRole('heading', { level: 1, name: entity === 'album' ? 'Quiet Rooms' : 'North Window' })).toBeVisible();
		const sibling = await context.newPage();
		await sibling.goto('/login');
		await sibling.evaluate(() => localStorage.setItem('nntune.library.auth-transition.v1', crypto.randomUUID()));
		await expect(page.getByRole('article').getByRole('link', { name: 'Sign in', exact: true })).toBeVisible();
		await expect(page.getByRole('heading', { name: 'Quiet Rooms', level: 1 })).toHaveCount(0);
		await expect(page.getByRole('heading', { name: 'North Window', level: 1 })).toHaveCount(0);
		await sibling.close();
	});

	test(`${entity} rejects an old-account response completing after an auth transition`, async ({ page, context }) => {
		await catalogue(page);
		const sibling = await context.newPage();
		await sibling.goto('/login');
		let releaseResponse!: () => void;
		let requestStarted!: () => void;
		const started = new Promise<void>((resolve) => { requestStarted = resolve; });
		const gate = new Promise<void>((resolve) => { releaseResponse = resolve; });
		await page.route(entity === 'album' ? '**/v1/releases/*' : '**/v1/artists/*', async (route) => {
			requestStarted();
			await gate;
			await route.fulfill({ contentType: 'application/json', body: JSON.stringify(entity === 'album' ? { release, tracks: [track] } : { artist, releases: [release], truncated: false }) });
		});
		const navigation = page.goto(`/${entity}/${entity === 'album' ? albumId : artistId}`);
		await started;
		// Deliver synchronously while the loader is in flight. The preceding
		// test covers real sibling-tab delivery; this case pins response order.
		await page.evaluate(() => window.dispatchEvent(new StorageEvent('storage', {
			key: 'nntune.library.auth-transition.v1', newValue: 'changed-account'
		})));
		releaseResponse();
		await navigation;
		await expect(page.getByRole('article').getByRole('link', { name: 'Sign in', exact: true })).toBeVisible();
		await expect(page.getByRole('heading', { name: entity === 'album' ? 'Quiet Rooms' : 'North Window', level: 1 })).toHaveCount(0);
		await sibling.close();
	});
}

test('track destinations resolve the exact disc without reading a library page', async ({ page }) => {
	await catalogue(page);
	const libraryRequests: string[] = [];
	page.on('request', (request) => {
		if (/\/v1\/library(?:\?|$)/.test(request.url())) libraryRequests.push(request.url());
	});
	await page.goto(`/album/${albumId}`);
	const link = page.getByRole('table', { name: 'Disc 2' }).getByRole('link', { name: 'First Light' });
	await link.focus();
	await page.keyboard.press('Enter');
	await expect(page).toHaveURL(`/track/${trackId}?release=${albumId}&medium=2&position=1`);
	await expect(page.getByRole('heading', { level: 1, name: 'First Light' })).toBeVisible();
	await expect(page.getByText('Disc 2 · Track 1', { exact: true })).toBeVisible();
	await expect(page.getByText('Track · Unknown', { exact: true })).toBeVisible();
	await expect(page.getByRole('button', { name: /play/i })).toHaveCount(0);
	await page.getByRole('article', { name: 'Track' }).getByRole('link', { name: 'Quiet Rooms', exact: true }).click();
	await expect(page).toHaveURL(`/album/${albumId}`);
	expect(libraryRequests).toEqual([]);
});

test('track rejects invalid and absent occurrence identities without a misleading destination', async ({ page }) => {
	await catalogue(page);
	for (const query of ['medium=1&position=1', `release=${albumId}&medium=1&position=1&position=2`, `release=${albumId}&medium=3&position=1`]) {
		await page.goto(`/track/${trackId}?${query}`);
		await expect(page.getByText('This recording occurrence is not available in your catalogue.')).toBeVisible();
		await expect(page.getByRole('heading', { level: 1, name: 'First Light' })).toHaveCount(0);
	}
	await page.route('**/v1/releases/*', (route) => route.fulfill({ contentType: 'application/json', body: JSON.stringify({ release, tracks: [track, track] }) }));
	await page.goto(`/album/${albumId}`);
	await expect(page.getByRole('heading', { level: 1, name: 'Quiet Rooms' })).toBeVisible();
	await expect(page.locator('.catalogue-tracks a')).toHaveCount(0);
});

test('track preserves signed-out, absent and retryable failures', async ({ page }) => {
	let status = 401;
	await page.route('**/v1/**', (route) => route.fulfill({ status, contentType: 'application/json', body: JSON.stringify({ code: 'unavailable', message: 'diagnostic', retryable: false }) }));
	await page.goto(`/track/${trackId}?release=${albumId}&medium=1&position=1`);
	await expect(page.getByRole('article', { name: 'Track' }).getByRole('link', { name: 'Sign in', exact: true })).toBeVisible();
	status = 404;
	await page.reload();
	await expect(page.getByText('This recording occurrence is not available in your catalogue.')).toBeVisible();
	status = 503;
	await page.reload();
	await expect(page.getByRole('button', { name: 'Retry' })).toBeVisible();
	await catalogue(page);
	await page.getByRole('button', { name: 'Retry' }).click();
	await expect(page.getByRole('heading', { level: 1, name: 'First Light' })).toBeVisible();
});

test('track clears account data and fences an old response across auth transitions', async ({ page }) => {
	await catalogue(page);
	const url = `/track/${trackId}?release=${albumId}&medium=1&position=1`;
	await page.goto(url);
	await expect(page.getByRole('heading', { level: 1, name: 'First Light' })).toBeVisible();
	await page.evaluate(() => window.dispatchEvent(new StorageEvent('storage', {
		key: 'nntune.library.auth-transition.v1', newValue: 'next-account'
	})));
	await expect(page.getByRole('article', { name: 'Track' }).getByRole('link', { name: 'Sign in', exact: true })).toBeVisible();
	await expect(page.getByRole('heading', { level: 1, name: 'First Light' })).toHaveCount(0);
	let releaseResponse!: () => void;
	let requestStarted!: () => void;
	const gate = new Promise<void>((resolve) => { releaseResponse = resolve; });
	const started = new Promise<void>((resolve) => { requestStarted = resolve; });
	await page.route('**/v1/releases/*', async (route) => {
		requestStarted();
		await gate;
		await route.fulfill({ contentType: 'application/json', body: JSON.stringify({ release, tracks: [track] }) });
	});
	const navigation = page.reload();
	await started;
	await page.evaluate(() => window.dispatchEvent(new StorageEvent('storage', {
		key: 'nntune.library.auth-transition.v1', newValue: 'third-account'
	})));
	releaseResponse();
	await navigation;
	await expect(page.getByRole('article', { name: 'Track' }).getByRole('link', { name: 'Sign in', exact: true })).toBeVisible();
	await expect(page.getByRole('heading', { level: 1, name: 'First Light' })).toHaveCount(0);
});

test('a capped artist discography is labelled incomplete', async ({ page }) => {
	await catalogue(page);
	await page.route('**/v1/artists/*', (route) => route.fulfill({ contentType: 'application/json', body: JSON.stringify({ artist, releases: [release], truncated: true }) }));
	await page.goto(`/artist/${artistId}`);
	await expect(page.getByRole('article').getByRole('status')).toContainText('This discography may be incomplete.');
});
