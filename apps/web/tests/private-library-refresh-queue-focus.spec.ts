import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { fixtureCookie, hasFixtureSession, observeMediaSources, test } from './helpers/private-music-fixture.js';

test('an invited Library refresh recovers focus when a queued track temporarily loses Reorder', async ({
	page, context, origin
}) => {
	const { library, player } = PRIVATE_MUSIC;
	const listener = 'synthetic-listener';
	const binding = 'A'.repeat(43);
	const verifier = '/v1/me/likes/session';
	const selected = { id: '00000000-0000-4000-8000-000000000001', title: 'Synthetic Prelude' };
	const target = { id: '00000000-0000-4000-8000-000000000002', title: 'Synthetic Interlude' };
	const listingUrl = origin + library.api;
	const reply = Promise.withResolvers<void>();
	const playbackRequests: string[] = [];
	let listingReads = 0;

	await context.route('**/*', async (route) => {
		const request = route.request();
		const { origin: requestOrigin, pathname, search } = new URL(request.url());
		if (pathname === player.plays || pathname.startsWith(`${player.plays}/`) || request.resourceType() === 'media') {
			playbackRequests.push(pathname);
			return route.abort();
		}
		if (requestOrigin !== origin || request.method() !== 'GET') return route.abort();
		if (!search && (pathname === library.api || pathname === verifier)) {
			if (!hasFixtureSession(await request.headerValue('cookie'), listener)) return route.fulfill({ status: 401 });
			if (pathname === verifier) return route.fulfill({ json: { session_binding: binding } });
			const read = ++listingReads;
			if (read > 2) return route.fulfill({ status: 404 });
			if (read === 2) await reply.promise;
			return route.fulfill({
				headers: { 'x-nntune-session-binding': binding },
				json: [selected, target]
			});
		}
		if (pathname.startsWith('/api/') || pathname.startsWith('/v1/')) return route.fulfill({ status: 404 });
		return route.continue();
	});
	await context.addCookies([{ name: fixtureCookie, value: listener, url: origin, httpOnly: true }]);
	await page.goto(origin + library.path);
	const sources = await observeMediaSources(page);
	const libraryRows = page.getByTestId(library.item);
	const queueRows = page.getByTestId(player.queueItem);
	const current = page.locator(player.currentQueueItem);
	const targetRow = queueRows.and(page.locator(`[${player.queueId}="${target.id}"]`));
	const reorder = targetRow.getByRole('button', { name: `Reorder ${target.title}`, exact: true });
	const checkbox = targetRow.getByRole('checkbox', { name: `Select ${target.title} for queue actions`, exact: true });
	const refresh = page.getByRole('button', { name: library.refresh, exact: true });
	const loading = page.getByText('Loading your library…', { exact: true });

	await expect(libraryRows).toHaveCount(2);
	for (const track of [selected, target]) {
		await libraryRows.and(page.locator(`[${library.id}="${track.id}"]`))
			.getByRole('button', { name: track.title, exact: true }).click();
	}
	await expect(queueRows).toHaveCount(2);
	await expect(current).toHaveAttribute(player.queueId, selected.id);
	await checkbox.focus();
	await page.keyboard.press('Tab');
	await expect(reorder).toBeFocused();
	const heldRow = await targetRow.elementHandle();
	const heldRefresh = await refresh.elementHandle();

	const request = page.waitForRequest(listingUrl);
	const response = page.waitForResponse(listingUrl);
	try {
		// Start the ordinary Refresh handler without moving focus away from Reorder.
		await refresh.dispatchEvent('click');
		await request;
		await expect(loading).toBeVisible();
		await expect(reorder).toHaveCount(0);
		expect(await heldRow.evaluate((node) => node.isConnected)).toBe(true);
		expect(await heldRefresh.evaluate((node) => node.isConnected)).toBe(true);
		await expect(current).toHaveAttribute(player.queueId, selected.id);
	} finally {
		reply.resolve();
	}

	expect((await response).status()).toBe(200);
	await expect(loading).toHaveCount(0);
	expect(await heldRow.evaluate((node) => node.isConnected)).toBe(true);
	await expect(reorder).toBeEnabled();
	await expect(refresh).toBeFocused();
	await page.keyboard.press('Shift+Tab');
	await expect(page.getByRole('button', { name: 'Copy queue as text', exact: true })).toBeFocused();
	await page.keyboard.press('Tab');
	await expect(refresh).toBeFocused();
	await expect(current).toHaveAttribute(player.queueId, selected.id);
	expect(await queueRows.evaluateAll((rows, attribute) => rows.map((row) => row.getAttribute(attribute)), player.queueId))
		.toEqual([selected.id, target.id]);
	expect(listingReads).toBe(2);
	expect(playbackRequests).toEqual([]);
	expect(await sources.evaluate((seen) => [...seen])).toEqual([]);
});
