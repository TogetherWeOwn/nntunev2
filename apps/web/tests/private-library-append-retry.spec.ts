import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { fixtureCookie, hasFixtureSession, observeMediaSources, test } from './helpers/private-music-fixture.js';

for (const [retainSelection, title] of [
	[true, 'the invited Library can retry a refused append of one selected track'],
	[false, 'the invited Library clears the append refusal when the selected track disappears']
] as const) test(title, async ({ page, context, origin }) => {
	const { library, player } = PRIVATE_MUSIC;
	const listener = 'synthetic-listener';
	const binding = 'A'.repeat(43);
	const verifierPath = '/v1/me/likes/session';
	const selected = { id: '00000000-0000-4000-8000-000000000001', title: 'Harbor Dawn' };
	const neighbor = { id: '00000000-0000-4000-8000-000000000002', title: 'Field Dusk' };
	const refreshReply = Promise.withResolvers<void>();
	const playbackRequests: string[] = [];
	let listingReads = 0;

	await context.route('**/*', async (route) => {
		const request = route.request();
		const url = new URL(request.url());
		const path = url.pathname;
		if (path === player.plays || path.startsWith(`${player.plays}/`) || request.resourceType() === 'media') {
			playbackRequests.push(path);
			return route.abort();
		}
		if (url.origin !== origin || request.method() !== 'GET') return route.abort();
		if (!url.search && (path === library.api || path === verifierPath)) {
			if (!hasFixtureSession(await request.headerValue('cookie'), listener)) return route.fulfill({ status: 401 });
			if (path === verifierPath) return route.fulfill({ json: { session_binding: binding } });
			listingReads += 1;
			if (listingReads > 2) return route.fulfill({ status: 500 });
			if (listingReads === 2) await refreshReply.promise;
			const tracks = listingReads === 1 || retainSelection ? [selected, neighbor] : [neighbor];
			return route.fulfill({ headers: { 'x-nntune-session-binding': binding }, json: tracks });
		}
		if (path.startsWith('/api/') || path.startsWith('/v1/')) return route.fulfill({ status: 404 });
		return route.continue();
	});
	await context.addCookies([{ name: fixtureCookie, value: listener, url: origin, httpOnly: true }]);

	await page.goto(origin + library.path);
	const mediaSources = await observeMediaSources(page);
	const rows = page.getByTestId('library-list').getByTestId(library.item);
	const checkbox = (id: string) => rows.and(page.locator(`[${library.id}="${id}"]`)).getByRole('checkbox');
	const queue = page.getByTestId(player.queueItem);
	const append = page.getByRole('button', { name: 'Append selected', exact: true });
	const feedback = page.getByRole('status').and(page.getByTestId('append-result'));
	const loading = page.getByRole('status').filter({ hasText: 'Loading your library…' });

	await checkbox(selected.id).check();
	await expect(page.getByTestId('selection-count')).toHaveText('1 selected');
	await expect(queue).toHaveCount(0);
	expect(listingReads).toBe(1);

	try {
		await page.getByRole('button', { name: library.refresh, exact: true }).click();
		await expect(loading).toBeVisible();
		await expect.poll(() => listingReads).toBe(2);
		await append.click();
		await expect(feedback).toHaveText('Could not add selected tracks while the Library is refreshing. Try again when it finishes.');
		await expect(feedback).toBeVisible();
		await expect(queue).toHaveCount(0);
		await expect(checkbox(selected.id)).toBeChecked();
		await expect(checkbox(neighbor.id)).not.toBeChecked();
		await expect(append).toHaveCount(1);
		await expect(append).toBeEnabled();
		await expect(append).toBeFocused();
		await expect(loading).toBeVisible();
	} finally {
		refreshReply.resolve();
	}

	await expect(loading).toBeHidden();
	await expect(page.getByTestId('library-stale-badge')).toHaveCount(0);
	await expect(queue).toHaveCount(0);
	if (retainSelection) {
		await expect(feedback).toBeVisible();
		await expect(append).toBeFocused();
		await page.keyboard.press('Enter');
		await expect(feedback).toHaveText('Added 1 track (0 already queued)');
		await expect(queue).toHaveCount(1);
		await expect(queue).toBeVisible();
		await expect(queue).toHaveAttribute(player.queueId, selected.id);
		await expect(checkbox(selected.id)).toBeChecked();
	} else {
		await expect(checkbox(selected.id)).toHaveCount(0);
		await expect(append).toBeDisabled();
		await expect(page.getByTestId('append-result')).toHaveCount(0);
	}
	await expect(checkbox(neighbor.id)).toBeVisible();
	await expect(checkbox(neighbor.id)).not.toBeChecked();
	expect(listingReads).toBe(2);
	expect(await mediaSources.evaluate((sources) => [...sources].filter(Boolean))).toEqual([]);
	expect(playbackRequests).toEqual([]);
});
