import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { fixtureCookie, hasFixtureSession, test } from './helpers/private-music-fixture.js';

test('the invited Library stars only the invoked same-title track across Refresh', async ({
	page, context, origin
}) => {
	const { library, player } = PRIVATE_MUSIC;
	const listener = 'synthetic-favorite-listener';
	const binding = 'A'.repeat(43);
	const likesPath = '/v1/me/likes';
	const verifierPath = '/v1/me/likes/session';
	const tastePath = '/v1/me/taste/session';
	const title = 'Synthetic Echo';
	const privateTrack = (id: string) => ({ id, title, occurrence_key: `private:${id}`, like_target_id: null });
	const other = privateTrack('00000000-0000-4000-8000-000000000001');
	const target = privateTrack('00000000-0000-4000-8000-000000000002');
	const unexpectedRequests: string[] = [];
	let listingReads = 0;
	await context.route('**/*', async (route) => {
		const request = route.request();
		const url = new URL(request.url());
		const path = url.pathname;
		if (url.origin !== origin || request.method() !== 'GET' || request.resourceType() === 'media' ||
			path === player.plays || path.startsWith(`${player.plays}/`)) {
			unexpectedRequests.push(`${request.method()} ${path}`);
			return route.abort();
		}
		if (!url.search && [library.api, verifierPath, likesPath, tastePath].includes(path)) {
			if (!hasFixtureSession(await request.headerValue('cookie'), listener)) return route.fulfill({ status: 401 });
			if (path === verifierPath) return route.fulfill({ json: { session_binding: binding } });
			if (path !== library.api) return route.fulfill({ json: { session_binding: binding, items: [] } });
			listingReads++;
			return route.fulfill({
				headers: { 'x-nntune-session-binding': binding },
				json: listingReads === 1 ? [other, target] : [target, other]
			});
		}
		if (path.startsWith('/api/') || path.startsWith('/v1/')) return route.fulfill({ status: 404 });
		return route.continue();
	});
	await context.addCookies([{ name: fixtureCookie, value: listener, url: origin, httpOnly: true }]);
	await page.goto(origin + library.path);

	const rows = page.getByTestId('library-list').getByTestId(library.item);
	const rowIds = () => rows.evaluateAll((nodes, attribute) => nodes.map((node) => node.getAttribute(attribute)), library.id);
	const targetRow = rows.and(page.locator(`[${library.id}="${target.id}"]`));
	const otherRow = rows.and(page.locator(`[${library.id}="${other.id}"]`));
	const star = (pressed: boolean) => targetRow.getByRole('button', { name: `${pressed ? 'Unstar' : 'Star'} ${title}`, exact: true });
	const otherStar = otherRow.getByRole('button', { name: `Star ${title}`, exact: true });
	const targetSelection = targetRow.getByRole('checkbox', { name: `Select ${title}`, exact: true });
	const otherSelection = otherRow.getByRole('checkbox', { name: `Select ${title}`, exact: true });
	const favorites = page.getByTestId('favorite-item');
	const expectFavorite = async (pressed: boolean) => {
		await expect(star(pressed)).toHaveAttribute('aria-pressed', String(pressed));
		await expect(otherStar).toHaveAttribute('aria-pressed', 'false');
		await expect(otherSelection).toBeChecked();
		await expect(targetSelection).not.toBeChecked();
		await expect(favorites).toHaveCount(pressed ? 1 : 0);
		if (pressed) await expect(favorites).toHaveAttribute(library.id, target.id);
	};

	await expect.poll(rowIds).toEqual([other.id, target.id]);
	await expect(rows.getByRole('button', { name: title, exact: true })).toHaveCount(2);
	// Keep the other row checked so a row action cannot borrow the bulk selection.
	await otherSelection.check();
	await expectFavorite(false);
	const storageKey = await page.evaluate(() => {
		const scope = sessionStorage.getItem('nntune.library.auth-context.v1');
		if (!scope) throw new Error('Missing Library auth context');
		return `nntune.library.favorites.v1.${scope}`;
	});
	const writes = await page.evaluateHandle((key) => {
		const attempts: string[] = [];
		const setItem = Storage.prototype.setItem;
		Storage.prototype.setItem = function (name, value) {
			if (this === localStorage && name === key) {
				attempts.push(value);
				if (attempts.length === 1) throw new DOMException('Synthetic quota failure', 'QuotaExceededError');
			}
			return setItem.call(this, name, value);
		};
		return attempts;
	}, storageKey);
	const persisted = () => page.evaluate((key) => localStorage.getItem(key), storageKey);
	const saved = JSON.stringify([target.id]);
	await star(false).click();
	await expect.poll(() => writes.jsonValue()).toEqual([saved]);
	await expectFavorite(true);
	expect(await persisted()).toBeNull();
	await star(true).click();
	await expectFavorite(false);
	await star(false).click();
	await expect.poll(persisted).toBe(saved);
	await expectFavorite(true);
	expect(listingReads).toBe(1);

	const refreshed = page.waitForResponse(origin + library.api);
	await page.getByRole('button', { name: library.refresh, exact: true }).click();
	const response = await refreshed;
	expect(response.status()).toBe(200);
	expect(response.request().method()).toBe('GET');
	await expect.poll(rowIds).toEqual([target.id, other.id]);
	await expectFavorite(true);
	expect(await persisted()).toBe(saved);

	await star(true).click();
	await expect.poll(persisted).toBe('[]');
	await expectFavorite(false);
	expect(await writes.jsonValue()).toEqual([saved, saved, '[]']);
	await expect(page.getByTestId(player.queueItem)).toHaveCount(0);
	await expect(page).toHaveURL(origin + library.path);
	expect(listingReads).toBe(2);
	expect(unexpectedRequests).toEqual([]);
});
