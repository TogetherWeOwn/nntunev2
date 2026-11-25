import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { fixtureCookie, hasFixtureSession, test } from './helpers/private-music-fixture.js';

test('Retry refreshes the invited Library Find results and keeps the current track', async ({
	page, context, origin
}) => {
	const { library, player } = PRIVATE_MUSIC;
	const listener = 'synthetic-listener';
	const binding = 'A'.repeat(43);
	const verifierPath = '/v1/me/likes/session';
	const first = { id: '00000000-0000-4000-8000-000000000001', title: 'Harbor Dawn' };
	const selected = { id: '00000000-0000-4000-8000-000000000002', title: 'Harbor Lights' };
	const unrelated = { id: '00000000-0000-4000-8000-000000000003', title: 'Field Echo' };
	const added = { id: '00000000-0000-4000-8000-000000000004', title: 'Harbor Dusk' };
	const initial = [first, selected, unrelated];
	const refreshed = [added, selected, { ...first, title: 'Morning Tide' }, unrelated];
	let listingReads = 0;

	await context.route('**/*', async (route) => {
		const request = route.request();
		const { origin: requestOrigin, pathname, search } = new URL(request.url());
		if (requestOrigin !== origin || request.method() !== 'GET') return route.abort();
		if (!search && (pathname === library.api || pathname === verifierPath)) {
			if (!hasFixtureSession(await request.headerValue('cookie'), listener)) return route.fulfill({ status: 401 });
			if (pathname === verifierPath) return route.fulfill({ json: { session_binding: binding } });
			const read = ++listingReads;
			if (read === 2) return route.fulfill({ status: 503, json: {} });
			if (read > 3) return route.fulfill({ status: 500 });
			return route.fulfill({
				headers: { 'x-nntune-session-binding': binding },
				json: read === 1 ? initial : refreshed
			});
		}
		if (pathname.startsWith('/api/') || pathname.startsWith('/v1/')) return route.fulfill({ status: 404 });
		return route.continue();
	});
	await context.addCookies([{ name: fixtureCookie, value: listener, url: origin, httpOnly: true }]);

	const main = page.getByRole('main');
	const rows = main.getByTestId(library.item);
	const trackButton = (id: string) => rows.and(page.locator(`[${library.id}="${id}"]`))
		.locator(library.select);
	const expectTracks = async (tracks: typeof initial, queuedIds: string[] = []) => {
		await expect(rows).toHaveCount(tracks.length);
		for (const { id, title } of tracks) {
			const button = trackButton(id);
			await expect(button).toBeVisible();
			await expect(button).toHaveAccessibleName(queuedIds.includes(id) ? `${title} — Queued` : title);
		}
	};
	const find = main.getByRole('searchbox', { name: 'Find in loaded tracks', exact: true });
	const refresh = main.getByRole('button', { name: library.refresh, exact: true });
	const retry = main.getByRole('button', { name: 'Retry', exact: true });
	const stale = main.getByRole('status').and(main.getByTestId('library-stale-badge'));
	const current = page.locator(player.currentQueueItem);

	await page.goto(origin + library.path);
	await expectTracks(initial);
	await find.fill('harbor');
	await expectTracks([first, selected]);
	await trackButton(first.id).click();
	await trackButton(selected.id).click();
	await page.getByTestId(player.queueItem)
		.getByRole('button', { name: `Select ${selected.title}`, exact: true }).click();
	await expect(current).toHaveAttribute(player.queueId, selected.id);
	expect(listingReads).toBe(1);

	await refresh.click();
	await expect(retry).toBeVisible();
	await expect(stale).toBeVisible();
	await expect(find).toBeVisible();
	await expect(find).toHaveValue('harbor');
	await expectTracks([first, selected], [first.id, selected.id]);
	await expect(current).toHaveAttribute(player.queueId, selected.id);
	expect(listingReads).toBe(2);

	await retry.click();
	await expectTracks([added, selected], [selected.id]);
	await expect(find).toBeVisible();
	await expect(find).toHaveValue('harbor');
	await expect(refresh).toBeVisible();
	await expect(stale).toBeHidden();
	await expect(current).toHaveAttribute(player.queueId, selected.id);
	expect(listingReads).toBe(3);
});
