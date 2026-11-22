import { expect, test } from '@playwright/test';

const current = 'A'.repeat(43);
const contextKey = 'nntune.library.auth-context.v1';
const favoritesKey = 'nntune.library.favorites.v1.bound';
const verifier = '**/v1/me/likes/session';
const mbid = '11111111-1111-4111-8111-000000000001';
test.beforeEach(({ page }) => page.addInitScript(({ current, contextKey, favoritesKey }) => {
	sessionStorage.setItem(contextKey, 'bound');
	sessionStorage.setItem('nntune.library.session-binding.v1', JSON.stringify(['bound', current]));
	localStorage.setItem(favoritesKey, '["track"]');
	localStorage.setItem('nntune.library.recent-finds.v1.bound', '["Scoped query"]');
	Object.defineProperty(navigator, 'clipboard', { value: undefined });
}, { current, contextKey, favoritesKey }));

for (const view of ['Library', 'Liked']) test(`${view} keeps a local favorite edit in its original visit`, async ({ page }) => {
	await page.route('**/api/library', (route) => route.fulfill({ headers: { 'x-nntune-session-binding': current },
		json: [{ id: 'track', title: 'Scoped track' }] }));
	await page.route(verifier, (route) => route.fulfill({ json: { session_binding: current } }));
	await page.route('**/v1/me/likes', (route) => route.fulfill({ json: { session_binding: current, items: [] } }));
	await page.route('**/v1/me/taste/session', (route) => route.fulfill({ json: { session_binding: current, items: [] } }));
	const liked = view === 'Liked';
	await page.goto(liked ? '/library/liked' : '/library');
	const edit = liked ? page.getByRole('button', { name: 'Remove Scoped track from Liked' })
		: page.getByTestId('favorites-section').getByRole('button', { name: 'Unstar Scoped track', exact: true });
	await expect(edit).toBeVisible();
	const held = liked && await page.evaluateHandle(async (key) => {
		const acquired = Promise.withResolvers<void>(), release = Promise.withResolvers<void>();
		const finished = navigator.locks.request(key, () => {
			acquired.resolve();
			return release.promise;
		});
		await acquired.promise;
		return { release: release.resolve, finished };
	}, favoritesKey);
	if (held) {
		await edit.click();
		await expect(edit).toHaveCount(0);
	}
	await page.evaluate(() => {
		const key = 'nntune.auth.generation.v1';
		localStorage.setItem(key, 'replacement-generation');
		window.dispatchEvent(new StorageEvent('storage', { key }));
	});
	const stored = () => page.evaluate((key) => localStorage.getItem(key), favoritesKey);
	if (held) {
		await held.evaluate(async ({ release, finished }, key) => {
			release();
			await finished;
			await navigator.locks.request(key, () => {});
		}, favoritesKey);
		expect(await stored()).toBe('["track"]');
		await page.getByRole('button', { name: 'Retry', exact: true }).click();
		await expect.poll(stored).toBe('[]');
	} else {
		await edit.click();
		await expect(edit).toHaveCount(0);
		expect(await stored()).toBe('["track"]');
	}
});

test('a verifier outage allows Retry; replacement retires local player and Undo', async ({ page }) => {
	let binding = current, failNext = false, verifies = 0;
	await page.route('**/api/library', (r) => r.fulfill({ headers: { 'x-nntune-session-binding': binding },
		json: [{ id: 'track', title: 'Scoped track', streamUrl: '/api/demo/audio' }] }));
	await page.route(verifier, (r) => {
		const status = failNext ? 503 : 200;
		failNext = false;
		verifies++;
		return r.fulfill({ status, json: { session_binding: binding } });
	});
	await page.goto('/library');
	const retry = page.getByRole('button', { name: 'Retry', exact: true });
	await page.getByRole('button', { name: 'Play displayed from the top' }).click();
	const time = () => page.getByTestId('audio-player').evaluate((el: HTMLAudioElement) => el.currentTime);
	await expect.poll(time).toBeGreaterThan(0);
	const filter = page.getByRole('searchbox', { name: 'Filter queue' });
	await filter.fill('Scoped');
	const failedSave = await page.evaluateHandle((key) => {
		const write = Storage.prototype.setItem, failed = Promise.withResolvers<void>();
		Storage.prototype.setItem = function (name, value) {
			if (name !== key) return write.call(this, name, value);
			Storage.prototype.setItem = write;
			failed.resolve();
			throw new DOMException('Favorites quota', 'QuotaExceededError');
		};
		return { settled: failed.promise };
	}, favoritesKey);
	await page.getByTestId('favorites-section').getByRole('button', { name: 'Unstar Scoped track', exact: true }).click();
	await failedSave.evaluate(({ settled }) => settled);
	failNext = true;
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(retry).toBeVisible();
	await expect(page.getByTestId('library-item')).toHaveCount(0);
	await expect(filter).toHaveValue('Scoped');
	await retry.click();
	await expect(page.getByTestId('favorites-empty')).toBeVisible();
	await expect(filter).toHaveValue('Scoped');
	expect(await page.evaluate((key) => localStorage.getItem(key), favoritesKey)).toBe('["track"]');
	failNext = true;
	const remove = page.getByRole('button', { name: 'Remove Scoped track from Liked' });
	await page.getByRole('link', { name: 'Liked', exact: true }).click();
	await expect(retry).toBeVisible();
	await expect(remove).toHaveCount(0);
	const before = await time();
	await expect.poll(time).toBeGreaterThan(before);
	await retry.click();
	await remove.click();
	const undo = page.getByRole('button', { name: 'Undo', exact: true });
	const retained = await undo.elementHandle();
	await page.getByRole('link', { name: 'Back to Your Library' }).click();
	await filter.fill('Scoped');
	await page.getByTestId('queue-copy-link').click();
	await expect(page.getByTestId('queue-share-fallback')).toBeVisible();
	await expect(undo).toBeVisible();
	await page.evaluate((key) => {
		localStorage.setItem(key, ' [ ] ');
		const setItem = Storage.prototype.setItem;
		Storage.prototype.setItem = function (key, value) {
			if (key === 'nntune.library.auth-transition.v1') document.body.dataset.authBroadcast = 'true';
			setItem.call(this, key, value);
		};
	}, favoritesKey);
	binding = 'E'.repeat(43);
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.getByRole('alert')).toContainText('Your session changed');
	for (const id of ['queue-item', 'library-item', 'queue-share-fallback', 'audio-player']) await expect(page.getByTestId(id)).toHaveCount(0);
	await expect(undo).toHaveCount(0);
	await retained!.evaluate((button, key) => {
		sessionStorage.setItem(key, 'replacement');
		// Invoke the original Svelte delegated handler, even after its control was detached.
		const click = Object.getOwnPropertySymbols(button).map((symbol) => Reflect.get(button, symbol)?.click).find((fn) => typeof fn === 'function');
		click.call(button, new MouseEvent('click'));
	}, contextKey);
	const persisted = await page.evaluate((key) => [localStorage.getItem(key),
		localStorage.getItem('nntune.library.favorites.v1.replacement'), document.body.dataset.authBroadcast], favoritesKey);
	expect(persisted).toEqual([' [ ] ', null, undefined]);
	expect(verifies).toBe(6);
	await retry.click();
	await expect(filter).toHaveValue('');
});

for (const view of ['Home', 'Albums', 'Artists']) {
	test(`a verifier outage offers Retry in ${view}`, async ({ page }) => {
		const fail = Promise.withResolvers<void>();
		await page.route('**/api/library', (r) => r.fulfill({ headers: { 'x-nntune-session-binding': current },
			json: [{ id: 'track', title: 'Scoped track' }] }));
		await page.route(verifier, (r) => r.fulfill({ json: { session_binding: current } }));
		await page.route(verifier, (r) => fail.promise.then(() => r.fulfill({ status: 503, json: {} })), { times: 1 });
		const checking = page.waitForRequest(verifier);
		const home = view === 'Home';
		await page.route('**/v1/releases', (r) => r.fulfill({ json: [{ mbid, title: 'Scoped album', release_type: 'Album' }] }));
		await page.route('**/v1/artists', (r) => r.fulfill({ json: [{ mbid, name: 'Scoped artist' }] }));
		await page.goto(home ? '/home' : '/library');
		if (!home) await page.getByRole('button', { name: view, exact: true }).click();
		const row = page.getByText(home ? 'Scoped track' : view === 'Albums' ? 'Scoped album' : 'Scoped artist', { exact: true });
		const gatedRegion = page.getByRole('region', { name: home ? 'Recent searches' : 'Library collections' });
		await checking;
		await expect(row.or(gatedRegion)).toHaveCount(0);
		fail.resolve();
		await expect(page.getByRole('alert')).toContainText('Could not verify your library session');
		await expect(row.or(gatedRegion)).toHaveCount(0);
		await page.getByRole('button', { name: 'Retry', exact: true }).click();
		await expect(row).toBeVisible();
		if (home) await expect(gatedRegion.getByRole('link', { name: 'Scoped query' })).toBeVisible();
	});
}
