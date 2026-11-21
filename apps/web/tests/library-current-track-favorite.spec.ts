import { expect, test, type Locator, type Page } from '@playwright/test';
const north = { id: 'fav-current-a', title: 'North Wind', streamUrl: '/test-audio/north' };
const south = { ...north, id: 'fav-current-b', title: 'South Wind' };
const east = { ...north, id: 'fav-current-c', title: 'East Wind' };
const contextKey = 'nntune.library.auth-context.v1';
const favoritesBase = 'nntune.library.favorites.v1';
const row = (page: Page, id: string) => page.locator(`[data-testid="library-item"][data-id="${id}"]`);
const queueRow = (page: Page, id: string) => page.locator(`[data-testid="queue-item"][data-track-id="${id}"]`);
const favorite = (page: Page) => page.getByTestId('player-favorite');
const feedback = (page: Page) => page.getByTestId('player-favorite-status').and(page.locator('[aria-live="polite"]'));
const scope = (page: Page) => page.evaluate((key) => sessionStorage.getItem(key), contextKey);
const star = (parent: Locator, track = north) => parent.getByRole('button', { name: new RegExp(`^(Unstar|Star) ${track.title}$`) });
async function openLibrary(page: Page) {
	await page.route('**/api/library', (route) => route.fulfill({ json: [north, south, east] }));
	await page.route('**/test-audio/**', () => {});
	await page.goto('/library');
	for (const track of [north, south]) await row(page, track.id).getByRole('button', { name: track.title, exact: true }).click();
	await expect(favorite(page)).toHaveAccessibleName('Favorite North Wind');
}
const stored = async (page: Page, context?: string | null) =>
	page.evaluate((key) => localStorage.getItem(key), `${favoritesBase}.${context ?? await scope(page)}`);
const holdWrites = async (page: Page, refuseNorth: boolean) =>
	page.evaluateHandle(({ key, refuseNorth, id }) => new Promise<() => Promise<string[]>>((ready) => {
		const setItem = Storage.prototype.setItem, writes: string[] = [];
		Storage.prototype.setItem = function (name, value) {
			if (name === key) writes.push(value);
			if (refuseNorth && name === key && JSON.parse(value).includes(id)) throw new DOMException('Refused', 'QuotaExceededError');
			setItem.call(this, name, value);
		};
		void navigator.locks.request(key, () => new Promise<void>((release) =>
			ready(() => { release(); return navigator.locks.request(key, () => writes); })));
	}), { key: `${favoritesBase}.${await scope(page)}`, refuseNorth, id: north.id });
for (const scenario of ['append then Refresh', 'Refresh fails after save', 'Refresh fails before save', 'refusal then save', 'membership round trip', 'selection round trip', 'leave and return']) {
	test(`held Favorite write: ${scenario}`, async ({ page }) => {
		await openLibrary(page);
		const refused = scenario === 'refusal then save', appended = scenario === 'append then Refresh';
		const failed = scenario.startsWith('Refresh fails'), lateSave = scenario.endsWith('before save');
		const changedSelection = scenario === 'selection round trip', left = scenario === 'leave and return', canceled = changedSelection || left;
		const refresh = Promise.withResolvers<void>(), ownerScope = await scope(page), decoder = await page.getByTestId('audio-player').elementHandle();
		const queueStar = star(queueRow(page, north.id)), southStar = star(row(page, south.id), south);
		if (refused) await southStar.click();
		const release = await holdWrites(page, refused);
		expect(await stored(page)).toBe(refused ? JSON.stringify([south.id]) : null);
		await favorite(page).press('Space');
		await expect(queueStar).toHaveAttribute('aria-pressed', 'true');
		if (appended) {
			await row(page, east.id).getByTestId('track-more').click();
			await page.getByRole('menuitem', { name: 'Add to queue', exact: true }).click();
		}
		if (appended || failed) {
			await page.route('**/api/library', (route) => refresh.promise.then(() => failed ? route.fulfill({ status: 500 }) : route.fallback()), { times: 1 });
			await page.getByRole('button', { name: 'Refresh', exact: true }).click();
			await expect(favorite(page)).toHaveCount(0);
		} else if (scenario === 'membership round trip') {
			for (const control of [queueStar, star(row(page, north.id))]) await control.click();
		} else if (left) {
			await page.getByRole('link', { name: 'Search', exact: true }).first().click();
			await expect(favorite(page)).toHaveCount(0);
			await page.goBack(); await expect(favorite(page)).toHaveAccessibleName('Favorite North Wind');
		} else if (changedSelection) {
			for (const { title } of [south, north]) await page.getByRole('button', { name: `Select ${title}`, exact: true }).click();
		}
		if (refused) await southStar.click();
		const saved = JSON.stringify([north.id]), writes = lateSave ? [] : await release.evaluate((unlock) => unlock());
		if (canceled || scenario === 'membership round trip') expect(writes).toEqual(canceled ? [] : [saved]);
		await expect.poll(async () => [await stored(page), await scope(page)]).toEqual([refused ? '[]' : canceled || lateSave ? null : saved, ownerScope]);
		if (appended || failed) await expect(feedback(page)).toBeEmpty();
		refresh.resolve();
		if (failed) {
			const stale = page.getByTestId('library-stale-badge');
			await expect(stale).toBeVisible();
			if (lateSave) expect(await release.evaluate((unlock) => unlock())).toEqual([saved]);
			await expect(feedback(page)).toBeEmpty();
			await page.getByRole('button', { name: 'Retry', exact: true }).click();
			await expect(stale).toHaveCount(0);
			expect(await stored(page)).toBe(saved);
		}
		for (const control of [favorite(page), queueStar, star(row(page, north.id))]) await expect(control).toHaveAttribute('aria-pressed', String(!canceled));
		await expect(feedback(page)).toHaveText(refused ? 'Starred North Wind for this visit.' : appended ? 'Starred North Wind.' : '');
		if (!changedSelection) expect(await page.getByTestId('audio-player').evaluate((node, original) => node === original, decoder)).toBe(true);
	});
}
const editDuringActivation = async (button: Locator, edit: Locator) =>
	button.evaluate((node, edit) => node.addEventListener('click',
		() => (edit as HTMLButtonElement).click(), { capture: true, once: true }), await edit.elementHandle());
for (const input of ['pointer', 'legacy chord', 'Space', 'click only']) {
	test(`${input} cannot star a replacement selection`, async ({ page }) => {
		const chord = input === 'legacy chord';
		if (chord) await page.addInitScript(() => window.addEventListener('click', (event) => {
			if (event.isTrusted && event.detail > 0) Object.setPrototypeOf(event, MouseEvent.prototype); // Keep native trust.
		}, true));
		await openLibrary(page);
		const button = favorite(page);
		const select = queueRow(page, south.id).getByRole('button', { name: 'Select South Wind', exact: true });
		const mouse = chord || input === 'pointer', replacement = input === 'Space' ? north : south;
		await button.evaluate((node) => {
			node.onpointerup = () => { node.dataset.pointerUp = 'yes'; };
			node.onclick = (event) => { node.dataset.click = !event.isTrusted ? 'synthetic' : 'pointerId' in event ? 'pointer' : 'legacy'; };
		});
		if (input === 'click only') {
			await editDuringActivation(button, select);
			await button.evaluate((node) => node.click());
		} else {
			if (mouse) {
				await button.hover(); await page.mouse.down();
				if (chord) await page.mouse.down({ button: 'right' });
			} else { await button.focus(); await page.keyboard.down('Space'); }
			await select.evaluate((node) => node.click());
			await expect(button).toHaveAccessibleName('Favorite South Wind');
			if (replacement === north) await queueRow(page, north.id).getByRole('button', { name: 'Select North Wind', exact: true }).evaluate((node) => node.click());
			if (replacement === north) await expect(button).toHaveAccessibleName('Favorite North Wind');
			if (mouse) { await button.hover(); await page.mouse.up(); }
			else await page.keyboard.up('Space');
		}
		await expect(button).toHaveAttribute('data-click', input === 'click only' ? 'synthetic' : chord ? 'legacy' : /^(pointer|legacy)$/);
		if (chord) { expect(await button.getAttribute('data-pointer-up')).toBeNull(); await page.mouse.up({ button: 'right' }); }
		await expect(button).toHaveAttribute('aria-pressed', 'false');
		await expect(feedback(page)).toBeEmpty();
		expect(await stored(page)).toBeNull();
		await (input === 'click only' ? button.evaluate((node) => node.click()) : button.click());
		await expect.poll(() => stored(page)).toBe(JSON.stringify([replacement.id]));
		await expect(page.locator('[aria-live="polite"]').filter({ hasText: `Starred ${replacement.title}.` })).toHaveCount(1);
		await expect(page.getByRole('region', { name: 'Player', exact: true }).getByRole('status')).toHaveCount(1);
	});
}
test('Favorite retires a press across reconciled membership and a same-click Library edit', async ({ page }) => {
	await openLibrary(page);
	const key = `${favoritesBase}.${await scope(page)}`;
	await favorite(page).focus(); await page.keyboard.down('Space');
	for (const [ids, pressed] of [[[north.id], 'true'], [[], 'false']] as const) {
		await page.evaluate(({ key, ids }) => navigator.locks.request(key, () => localStorage.setItem(key, JSON.stringify(ids))), { key, ids });
		await star(row(page, south.id), south).evaluate((button) => button.click());
		await expect(favorite(page)).toHaveAttribute('aria-pressed', pressed);
	}
	await expect(favorite(page)).toBeFocused();
	await page.keyboard.up('Space');
	await page.evaluate((key) => navigator.locks.request(key, () => {}), key);
	expect(await stored(page)).toBe('[]');
	await editDuringActivation(favorite(page), star(row(page, north.id)));
	await favorite(page).press('Space');
	await expect.poll(() => stored(page)).toBe(JSON.stringify([north.id]));
	await expect(feedback(page)).toBeEmpty();
});
test('authoritative Refresh recovers a missing scope, not an arbitrary replacement', async ({ page }) => {
	await page.addInitScript((key) => {
		const setItem = Storage.prototype.setItem;
		Storage.prototype.setItem = function (name, value) { if (name === key) throw new DOMException('Refused'); setItem.call(this, name, value); };
		window.addEventListener('restore-storage', () => { Storage.prototype.setItem = setItem; });
	}, contextKey);
	await openLibrary(page);
	expect(await scope(page)).toBeNull();
	await favorite(page).click();
	await expect(feedback(page)).toHaveText('Starred North Wind for this visit.');
	await page.evaluate(() => window.dispatchEvent(new Event('restore-storage')));
	const refresh = page.getByRole('button', { name: 'Refresh', exact: true });
	await refresh.click();
	for (const control of [favorite(page), star(row(page, north.id)), star(queueRow(page, north.id))]) await expect(control).toHaveAttribute('aria-pressed', 'true');
	const retired = await scope(page);
	expect([Boolean(retired), await stored(page, retired)]).toEqual([true, null]);
	await favorite(page).evaluate((button, key) => button.addEventListener('click',
		() => sessionStorage.setItem(key, 'replacement-context'), { capture: true, once: true }), contextKey);
	await favorite(page).click();
	await page.route('**/api/library', (route) => route.fulfill({ json: [north, south, { ...east, id: 'refreshed' }] }), { times: 1 });
	await refresh.click();
	await expect(row(page, 'refreshed')).toBeVisible();
	await expect(favorite(page)).toHaveCount(0);
	await expect(feedback(page)).toBeEmpty();
	expect(await Promise.all([stored(page, retired), stored(page)])).toEqual([null, null]);
	await page.evaluate(() => window.dispatchEvent(new StorageEvent('storage', { key: 'nntune.library.auth-transition.v1' })));
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
});
