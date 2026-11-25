import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { fixtureCookie, hasFixtureSession, test } from './helpers/private-music-fixture.js';

test('an invitee can tab to a track, queue it, and return focus to its actions', async ({
	page, context, origin
}) => {
	const { library, player } = PRIVATE_MUSIC;
	const listener = 'synthetic-listener';
	const binding = 'A'.repeat(43);
	const verifierPath = '/v1/me/likes/session';
	const other = { id: '00000000-0000-4000-8000-000000000001', title: 'Synthetic Prelude' };
	const target = { id: '00000000-0000-4000-8000-000000000002', title: 'Synthetic Interlude' };
	const playbackRequests: string[] = [];
	await context.route('**/*', async (route) => {
		const request = route.request();
		const url = new URL(request.url());
		const path = url.pathname;
		if (path === player.plays || path.startsWith(`${player.plays}/`) || request.resourceType() === 'media') {
			playbackRequests.push(path);
			return route.abort();
		}
		if (url.origin !== origin || request.method() !== 'GET') return route.abort();
		if (path === library.api || path === verifierPath) {
			if (!hasFixtureSession(await request.headerValue('cookie'), listener)) return route.fulfill({ status: 401 });
			return path === library.api
				? route.fulfill({ headers: { 'x-nntune-session-binding': binding }, json: [other, target] })
				: route.fulfill({ json: { session_binding: binding } });
		}
		if (path.startsWith('/api/') || path.startsWith('/v1/')) return route.fulfill({ status: 404 });
		return route.continue();
	});
	await context.addCookies([{ name: fixtureCookie, value: listener, url: origin, httpOnly: true }]);

	await page.goto(origin + library.path);
	const rows = page.getByRole('main').getByTestId(library.item);
	await expect(rows).toHaveCount(2);
	const select = page.getByRole('checkbox', { name: `Select ${target.title}`, exact: true });
	const row = rows.filter({ has: select });
	await expect(row).toHaveAttribute(library.id, target.id);
	const more = row.getByRole('button', { name: `More actions for ${target.title}`, exact: true });
	const queue = page.getByTestId(player.queueItem);
	await expect(queue).toHaveCount(0);

	// Use the browser's focus sequence without pinning the number of stops in the shell.
	for (let tabs = 0; tabs < 60 && !(await select.evaluate((element) => element.matches(':focus'))); tabs++) {
		await page.keyboard.press('Tab');
	}
	await expect(select).toBeFocused();
	await page.keyboard.press('Tab');
	await expect(row.getByRole('button', { name: target.title, exact: true })).toBeFocused();
	await page.keyboard.press('Tab');
	await expect(more).toBeFocused();
	await page.keyboard.press('Enter');
	await expect(page.getByRole('menuitem', { name: 'Play now', exact: true })).toBeFocused();
	await page.keyboard.press('ArrowDown');
	await expect(page.getByRole('menuitem', { name: 'Play next', exact: true })).toBeFocused();
	await page.keyboard.press('ArrowDown');
	await expect(page.getByRole('menuitem', { name: 'Add to queue', exact: true })).toBeFocused();
	await page.keyboard.press('Enter');

	await expect(queue).toHaveCount(1);
	await expect(queue).toBeVisible();
	await expect(queue).toHaveAttribute(player.queueId, target.id);
	await expect(page.getByRole('menuitem', { name: 'Add to queue', exact: true })).toBeHidden();
	await expect(more).toBeFocused();
	await expect(page.getByTestId('queue-announcement')).toHaveText('Added 1 track (0 already queued)');
	await expect(page.locator('audio[src]:not([src=""]), audio source[src]:not([src=""])')).toHaveCount(0);
	expect(playbackRequests).toEqual([]);
});
