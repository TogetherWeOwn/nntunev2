import { expect, test, type Locator, type Page } from '@playwright/test';

const north = { id: 'fav-current-a', title: 'North Wind', streamUrl: '/test-audio/north' };
const south = { ...north, id: 'fav-current-b', title: 'South Wind' };
const east = { ...north, id: 'fav-current-c', title: 'East Wind' };
const contextKey = 'nntune.library.auth-context.v1';
const favoritesBase = 'nntune.library.favorites.v1';
const row = (page: Page, id: string) => page.locator(`[data-testid="library-item"][data-id="${id}"]`);
const queueRow = (page: Page, id: string) => page.locator(`[data-testid="queue-item"][data-track-id="${id}"]`);
const favorite = (page: Page) => page.getByTestId('player-favorite');
const feedback = (page: Page) => page.getByTestId('player-favorite-status');
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
async function holdWrites(page: Page, refuseNorth = false) {
	return page.evaluateHandle(({ key, refuseNorth, id }) => new Promise<() => Promise<void>>((ready) => {
		const setItem = Storage.prototype.setItem;
		Storage.prototype.setItem = function (name, value) {
			if (refuseNorth && name === key && JSON.parse(value).includes(id)) throw new DOMException('Refused', 'QuotaExceededError');
			setItem.call(this, name, value);
		};
		void navigator.locks.request(key, () => new Promise<void>((release) =>
			ready(() => { release(); return navigator.locks.request(key, () => {}); })));
	}), { key: `${favoritesBase}.${await scope(page)}`, refuseNorth, id: north.id });
}
for (const scenario of ['append', 'refusal then save', 'membership round trip', 'leave and return']) {
	test(`held Favorite write: ${scenario}`, async ({ page }) => {
		await openLibrary(page);
		const refused = scenario === 'refusal then save';
		const [decoder, ownerScope] = await Promise.all([page.getByTestId('audio-player').elementHandle(), scope(page)]);
		const queueStar = star(queueRow(page, north.id));
		const southStar = star(row(page, south.id), south);
		if (refused) {
			await southStar.click();
			await expect.poll(() => stored(page)).toBe(JSON.stringify([south.id]));
		}
		const release = await holdWrites(page, refused);
		await favorite(page).press('Space');
		await expect(queueStar).toHaveAttribute('aria-pressed', 'true');
		if (scenario === 'append') {
			await row(page, east.id).getByTestId('track-more').click();
			await page.getByRole('menuitem', { name: 'Add to queue', exact: true }).click();
			await expect(queueRow(page, east.id)).toBeVisible();
		} else if (scenario === 'membership round trip') {
			await queueStar.click();
			await star(row(page, north.id)).click();
		} else if (scenario === 'leave and return') {
			await page.getByRole('link', { name: 'Search', exact: true }).first().click();
			await expect(favorite(page)).toHaveCount(0);
			await page.goBack();
			await expect(favorite(page)).toHaveAccessibleName('Favorite North Wind');
		}
		await expect(feedback(page)).toBeEmpty();
		await release.evaluate((unlock) => unlock());
		if (refused) {
			await expect(feedback(page)).toHaveText('Starred North Wind for this visit.');
			await southStar.click();
			await expect.poll(() => stored(page)).toBe('[]');
		}
		const left = scenario === 'leave and return';
		expect([await stored(page), await scope(page)]).toEqual([refused ? '[]' : left ? null : JSON.stringify([north.id]), ownerScope]);
		for (const control of [favorite(page), queueStar, star(row(page, north.id))])
			await expect(control).toHaveAttribute('aria-pressed', String(!left));
		await expect(feedback(page)).toHaveText(refused ? 'Starred North Wind for this visit.' : scenario === 'append' ? 'Starred North Wind.' : '');
		await expect(page.getByTestId('now-playing')).toContainText(north.title);
		await expect(page.getByTestId('player-next')).toBeEnabled();
		expect(await page.getByTestId('audio-player').evaluate((node, original) => node === original, decoder)).toBe(true);
	});
}
async function editDuringActivation(button: Locator, edit: Locator) {
	await button.evaluate((node, edit) => node.addEventListener('click',
		() => (edit as HTMLButtonElement).click(), { capture: true, once: true }), await edit.elementHandle());
}

for (const input of ['pointer', 'legacy chord', 'Space', 'click only']) {
	test(`${input} cannot star a replacement selection`, async ({ page }) => {
		const chord = input === 'legacy chord';
		if (chord) await page.addInitScript(() => window.addEventListener('click', (event) => {
			// Model a legacy browser without making the click untrusted.
			if (event.isTrusted && event.detail > 0) Object.setPrototypeOf(event, MouseEvent.prototype);
		}, true));
		await openLibrary(page);
		const button = favorite(page);
		const select = queueRow(page, south.id).getByRole('button', { name: 'Select South Wind', exact: true });
		const mouse = chord || input === 'pointer';
		const replacement = input === 'Space' ? north : south;
		await button.evaluate((node) => {
			node.addEventListener('pointerup', () => { node.dataset.pointerUp = 'yes'; });
			node.addEventListener('click', (event) => {
				node.dataset.click = !event.isTrusted ? 'synthetic' : 'pointerId' in event ? 'pointer' : 'legacy';
			});
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
		if (input === 'click only') await button.evaluate((node) => node.click());
		else await button.click();
		await expect.poll(() => stored(page)).toBe(JSON.stringify([replacement.id]));
		await expect(button).toHaveAttribute('aria-pressed', 'true');
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
	await page.evaluate(() => window.dispatchEvent(new Event('restore-storage')));
	const refresh = page.getByRole('button', { name: 'Refresh', exact: true });
	await refresh.click();
	await expect.poll(() => scope(page)).not.toBeNull();
	const retired = await scope(page);
	await favorite(page).evaluate((button, key) => button.addEventListener('click',
		() => sessionStorage.setItem(key, 'replacement-context'), { capture: true, once: true }), contextKey);
	await favorite(page).click();
	await refresh.click();
	await expect(favorite(page)).toHaveCount(0);
	await expect(feedback(page)).toBeEmpty();
	expect(await Promise.all([stored(page, retired), stored(page)])).toEqual([null, null]);
	await page.evaluate(() => window.dispatchEvent(new StorageEvent('storage', { key: 'nntune.library.auth-transition.v1' })));
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
});
