import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { fixtureCookie, hasFixtureSession, test } from './helpers/private-music-fixture.js';

test('an invited Library refresh keeps keyboard focus and actions on the moved track', async ({
	page, context, origin
}) => {
	const { library, player } = PRIVATE_MUSIC;
	const listener = 'synthetic-listener';
	const binding = 'A'.repeat(43);
	const target = { id: '00000000-0000-4000-8000-000000000001', title: 'Synthetic Prelude' };
	const other = { id: '00000000-0000-4000-8000-000000000002', title: 'Synthetic Interlude' };
	const reply = Promise.withResolvers<void>();
	let listingReads = 0;

	await context.route('**/*', async (route) => {
		const request = route.request();
		const { origin: requestOrigin, pathname, search } = new URL(request.url());
		if (requestOrigin !== origin || request.method() !== 'GET') return route.abort();
		if (!search && (pathname === library.api || pathname === '/v1/me/likes/session')) {
			if (!hasFixtureSession(await request.headerValue('cookie'), listener)) return route.fulfill({ status: 401 });
			if (pathname !== library.api) return route.fulfill({ json: { session_binding: binding } });
			const read = ++listingReads;
			if (read > 2) return route.fulfill({ status: 404 });
			if (read === 2) await reply.promise;
			return route.fulfill({
				headers: { 'x-nntune-session-binding': binding },
				json: read === 1 ? [target, other] : [other, target]
			});
		}
		if (pathname.startsWith('/api/') || pathname.startsWith('/v1/')) return route.fulfill({ status: 404 });
		return route.continue();
	});
	await context.addCookies([{ name: fixtureCookie, value: listener, url: origin, httpOnly: true }]);
	await page.goto(origin + library.path);

	const rows = page.getByTestId('library-list').getByTestId(library.item);
	const targetRow = rows.and(page.locator(`[${library.id}="${target.id}"]`));
	const more = targetRow.getByRole('button', { name: `More actions for ${target.title}`, exact: true });
	const rowIds = () => rows.evaluateAll((nodes, attribute) => nodes.map((row) => row.getAttribute(attribute)), library.id);
	const refresh = page.getByRole('button', { name: library.refresh, exact: true });
	const loading = page.getByText('Loading your library…', { exact: true });
	const queue = page.getByTestId(player.queueItem);

	await expect.poll(rowIds).toEqual([target.id, other.id]);
	await targetRow.getByRole('button', { name: target.title, exact: true }).focus();
	await page.keyboard.press('Tab');
	await expect(more).toBeFocused();
	await expect(queue).toHaveCount(0);
	const response = page.waitForResponse(origin + library.api);
	try {
		// Exercise the Refresh handler while leaving focus on the row.
		await refresh.dispatchEvent('click');
		await expect(loading).toBeVisible();
		await expect(more).toBeFocused();
	} finally {
		reply.resolve();
	}

	expect((await response).status()).toBe(200);
	await expect(loading).toBeHidden();
	await expect.poll(rowIds).toEqual([other.id, target.id]);
	await expect(more).toBeFocused();
	await page.keyboard.press('Enter');
	await expect(page.getByRole('menu', { name: `Actions for ${target.title}`, exact: true })).toBeVisible();
	await expect(page.getByRole('menuitem', { name: 'Play now', exact: true })).toBeFocused();
	await page.keyboard.press('ArrowDown');
	await page.keyboard.press('ArrowDown');
	await expect(page.getByRole('menuitem', { name: 'Add to queue', exact: true })).toBeFocused();
	await page.keyboard.press('Enter');
	await expect(queue).toHaveCount(1);
	await expect(queue).toHaveAttribute(player.queueId, target.id);
	expect(listingReads).toBe(2);
});
