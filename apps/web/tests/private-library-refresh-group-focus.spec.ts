import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { fixtureCookie, hasFixtureSession, observeMediaSources, test } from './helpers/private-music-fixture.js';

for (const { title, empty } of [
	{ title: 'an invited Library refresh recovers focus when the focused artist group disappears', empty: false },
	{ title: 'an invited Library refresh returns to Refresh when every group disappears', empty: true }
]) {
	test(title, async ({
		page, context, origin
	}) => {
		const { library, player } = PRIVATE_MUSIC;
		const listener = 'synthetic-listener';
		const binding = 'A'.repeat(43);
		const verifierPath = '/v1/me/likes/session';
		const removed = {
			id: '00000000-0000-4000-8000-000000000001',
			occurrence_key: '00000000-0000-4000-8000-000000000005:1',
			title: 'Synthetic Prelude',
			artist: 'Willow'
		};
		const survivor = { id: '00000000-0000-4000-8000-000000000002', title: 'Synthetic Finale', artist: 'Alder' };
		const kept = [
			{ ...survivor, occurrence_key: '00000000-0000-4000-8000-000000000003:1' },
			{ ...survivor, occurrence_key: '00000000-0000-4000-8000-000000000004:1' }
		];
		const refreshed = empty ? [] : kept;
		const reply = Promise.withResolvers<void>();
		const playbackRequests: string[] = [];
		let listings = 0;

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
				const listing = ++listings;
				if (listing > 2) return route.fulfill({ status: 404 });
				if (listing === 2) await reply.promise;
				return route.fulfill({
					headers: { 'x-nntune-session-binding': binding },
					json: listing === 1 ? [...kept, removed] : refreshed
				});
			}
			if (path.startsWith('/api/') || path.startsWith('/v1/')) return route.fulfill({ status: 404 });
			return route.continue();
		});
		await context.addCookies([{ name: fixtureCookie, value: listener, url: origin, httpOnly: true }]);
		await page.goto(origin + library.path);
		const sources = await observeMediaSources(page);
		const list = page.getByTestId('library-list');
		const rows = list.getByTestId(library.item);
		const goneRow = rows.and(page.locator(`[${library.id}="${removed.id}"]`));
		const keptRows = rows.and(page.locator(`[${library.id}="${survivor.id}"]`));
		const lostControl = goneRow.getByRole('button', { name: `More actions for ${removed.title}`, exact: true });
		const checkboxes = keptRows.getByRole('checkbox', { name: `Select ${survivor.title}`, exact: true });
		const grouping = page.getByRole('button', { name: 'Group by artist', exact: true });
		const refresh = page.getByRole('button', { name: library.refresh, exact: true });
		const append = page.getByRole('button', { name: 'Append selected', exact: true });
		const queue = page.getByTestId(player.queueItem);
		await expect(rows).toHaveCount(3);
		await grouping.press('Space');
		await expect(list.getByRole('heading', { name: removed.artist, exact: true })).toBeVisible();
		await expect(list.getByRole('heading', { name: survivor.artist, exact: true })).toBeVisible();
		await checkboxes.first().check();
		await expect(checkboxes.nth(1)).toBeChecked();
		await expect(append).toBeEnabled();
		await expect(queue).toHaveCount(0);

		const requested = page.waitForRequest(`${origin}${library.api}`);
		const response = page.waitForResponse(`${origin}${library.api}`);
		try {
			await refresh.press('Enter');
			await requested;
			await expect(refresh).toBeFocused();
			for (let tabs = 0; tabs < 24 && !(await lostControl.evaluate((element) => element.matches(':focus'))); tabs++) {
				await page.keyboard.press('Shift+Tab');
			}
			await expect(lostControl).toBeFocused();
			await expect(page.getByText('Loading your library…', { exact: true })).toBeVisible();
		} finally {
			reply.resolve();
		}

		expect((await response).status()).toBe(200);
		await expect(list.getByRole('heading', { name: removed.artist, exact: true })).toHaveCount(0);
		await expect(goneRow).toHaveCount(0);
		if (empty) {
			await expect(page.getByRole('status').filter({ hasText: 'Your library is empty.' })).toBeVisible();
			await expect(rows).toHaveCount(0);
			await expect(page.getByRole('combobox', { name: 'Show', exact: true })).toHaveCount(0);
			await expect(refresh).toBeFocused();
		} else {
			await expect(list.getByRole('heading', { name: survivor.artist, exact: true })).toBeVisible();
			await expect(keptRows).toHaveCount(2);
			await expect(grouping).toHaveAttribute('aria-pressed', 'true');
			// Willow followed both Alder occurrences. The second is its nearest survivor.
			const checkbox = checkboxes.nth(1);
			const play = keptRows.nth(1).getByRole('button', { name: `${survivor.title} — ${survivor.artist}`, exact: true });
			await expect(checkbox).toBeFocused();
			await page.keyboard.press('Tab');
			await expect(play).toBeFocused();
			await page.keyboard.press('Shift+Tab');
			await expect(checkbox).toBeFocused();
			await expect(checkbox).toBeChecked();
			await expect(page.getByTestId('selection-count')).toHaveText('1 selected');
			await expect(append).toBeEnabled();
		}
		await expect(queue).toHaveCount(0);
		expect(playbackRequests).toEqual([]);
		expect(await sources.evaluate((seen) => [...seen])).toEqual([]);
		expect(listings).toBe(2);
	});
}
