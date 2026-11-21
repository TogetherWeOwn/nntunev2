import { expect, test, type Locator, type Page } from '@playwright/test';

const tracks = [
	{ id: 'fav-current-a', title: 'North Wind', streamUrl: '/test-audio/north' },
	{ id: 'fav-current-b', title: 'South Wind', streamUrl: '/test-audio/south' }
];
const contextKey = 'nntune.library.auth-context.v1';
const transitionKey = 'nntune.library.auth-transition.v1';
const favoritesBase = 'nntune.library.favorites.v1';
const row = (page: Page, id: string) => page.locator(`[data-testid="library-item"][data-id="${id}"]`);
const queueRow = (page: Page, id: string) => page.locator(`[data-testid="queue-item"][data-track-id="${id}"]`);
const favorite = (page: Page) => page.getByTestId('player-favorite');
const feedback = (page: Page) => page.getByTestId('player-favorite-status');
async function openLibrary(page: Page) {
	await page.route('**/api/library', (route) => route.fulfill({ json: tracks }));
	await page.route('**/test-audio/**', () => {});
	await page.goto('/library');
	await expect(favorite(page)).toHaveCount(0);
	for (const { id, title } of tracks) await row(page, id).getByRole('button', { name: title, exact: true }).click();
	await expect(favorite(page)).toHaveAccessibleName('Favorite North Wind');
}
async function storedFavorites(page: Page, scope?: string | null) {
	return page.evaluate(({ contextKey, favoritesBase, scope }) =>
		localStorage.getItem(`${favoritesBase}.${scope ?? sessionStorage.getItem(contextKey)}`), { contextKey, favoritesBase, scope });
}
async function expectMembership(page: Page, id: string | null, announcement = '', visitOnly = false) {
	await expect(favorite(page)).toHaveAttribute('aria-pressed', String(id !== null));
	await expect(feedback(page)).toHaveText(announcement);
	await expect.poll(() => storedFavorites(page)).toBe(id === null || visitOnly ? null : JSON.stringify([id]));
}

test('the selected Favorite announces once after its held write, even with a later Library edit', async ({ page }) => {
	await openLibrary(page);
	const [north, south] = tracks;
	await queueRow(page, south.id).getByRole('button', { name: 'Select South Wind', exact: true }).click();
	const audio = page.getByTestId('audio-player');
	const decoder = await audio.elementHandle();
	const sameDecoder = () => audio.evaluate((node, original) => node === original, decoder);
	const nowPlaying = await page.getByTestId('now-playing').innerText();
	await expect(page.getByTestId('player-next')).toBeEnabled();
	await expect(queueRow(page, north.id).getByRole('button', { name: 'Star North Wind', exact: true })).toHaveAttribute('aria-pressed', 'false');
	const release = await page.evaluateHandle(({ contextKey, favoritesBase }) => new Promise<() => void>((ready) => {
		void navigator.locks.request(`${favoritesBase}.${sessionStorage.getItem(contextKey)}`, () => new Promise<void>((release) => ready(release)));
	}), { contextKey, favoritesBase });
	await favorite(page).and(page.getByRole('button', { name: 'Favorite South Wind', exact: true })).press('Space');
	await expect(favorite(page)).toBeFocused();
	await row(page, north.id).getByRole('button', { name: 'Star North Wind', exact: true }).click();
	await expect(feedback(page)).toBeEmpty();
	expect(await storedFavorites(page)).toBeNull();
	await release.evaluate((release) => release());
	await expect.poll(() => storedFavorites(page)).toBe(JSON.stringify([south.id, north.id]));
	await expect(favorite(page)).toHaveAttribute('aria-pressed', 'true');
	await expect(row(page, south.id).getByRole('button', { name: 'Unstar South Wind', exact: true })).toHaveAttribute('aria-pressed', 'true');
	await expect(page.locator('[aria-live="polite"]').filter({ hasText: /^Starred South Wind\.$/ })).toHaveCount(1);
	await expect(page.getByRole('region', { name: 'Player', exact: true }).getByRole('status')).toHaveCount(1);
	expect(await sameDecoder()).toBe(true);
	await expect(page.getByTestId('now-playing')).toHaveText(nowPlaying);
	await expect(page.getByTestId('player-next')).toBeEnabled();
	await queueRow(page, south.id).getByRole('button', { name: 'Unstar South Wind', exact: true, pressed: true }).click();
	await expect(favorite(page)).toHaveAttribute('aria-pressed', 'false');
	await expect(feedback(page)).toBeEmpty();
	await page.getByRole('link', { name: 'Search', exact: true }).first().click();
	await expect(page).toHaveURL(/\/search$/);
	await expect(favorite(page)).toHaveCount(0);
	expect(await sameDecoder()).toBe(true);
});

async function editDuringActivation(page: Page, edit: Locator) {
	await favorite(page).evaluate((button, edit) => button.addEventListener('click',
		() => (edit as HTMLButtonElement).click(), { capture: true, once: true }), await edit.elementHandle());
}

for (const input of ['pointer', 'legacy pointer', 'Space', 'click only']) {
	test(`${input} activation cannot star a replacement selection`, async ({ page }) => {
		if (input === 'legacy pointer') await page.addInitScript(() => window.addEventListener('click', (event) => {
			// Model older Safari: preserve trust and MouseEvent fields, but remove pointer identity.
			if (event.isTrusted && event.detail > 0) Object.setPrototypeOf(event, MouseEvent.prototype);
		}, true));
		await openLibrary(page);
		const button = favorite(page);
		await button.evaluate((node) => node.addEventListener('click', (event) => {
			node.dataset.favoriteClick = !event.isTrusted ? 'synthetic' : 'pointerId' in event ? 'pointer' : 'legacy';
		}, { once: true }));
		const select = queueRow(page, tracks[1].id).getByRole('button', { name: 'Select South Wind', exact: true });
		const pointer = input.endsWith('pointer');
		if (input === 'click only') {
			await editDuringActivation(page, select);
			await button.evaluate((node) => node.click());
		} else {
			if (pointer) {
				await button.hover(); await page.mouse.down();
				await page.evaluate(() => {
					const touch = { bubbles: true, isPrimary: true, pointerId: 42, pointerType: 'touch' };
					for (const type of ['pointerdown', 'pointercancel']) document.body.dispatchEvent(new PointerEvent(type, touch));
				});
			} else { await button.focus(); await page.keyboard.down('Space'); }
			await select.evaluate((node) => node.click());
			await expect(button).toHaveAccessibleName('Favorite South Wind');
			if (pointer) { await button.hover(); await page.mouse.up(); }
			else await page.keyboard.up('Space');
		}
		await expect(button).toHaveAttribute('data-favorite-click', input === 'click only' ? 'synthetic' : input === 'legacy pointer' ? 'legacy' : /^(pointer|legacy)$/);
		await expectMembership(page, null);
		if (input !== 'click only') return;
		await button.evaluate((node) => node.click());
		await expectMembership(page, tracks[1].id, 'Starred South Wind.');
		await expect(page.locator('[aria-live="polite"]').filter({ hasText: /^Starred South Wind\.$/ })).toHaveCount(1);
	});
}

test('a keyboard activation racing a Library edit does not toggle membership a second time', async ({ page }) => {
	await openLibrary(page);
	await editDuringActivation(page, row(page, tracks[0].id).getByRole('button', { name: 'Star North Wind', exact: true }));
	await favorite(page).press('Space');
	await expectMembership(page, tracks[0].id);
});

test('a rotated account refuses the stale button and an auth transition retires the control', async ({ page }) => {
	await openLibrary(page);
	const retired = await page.evaluate((key) => sessionStorage.getItem(key), contextKey);
	await favorite(page).evaluate((button, key) => button.addEventListener('click',
		() => sessionStorage.setItem(key, 'replacement-context'), { capture: true, once: true }), contextKey);
	await favorite(page).click();
	await expect(feedback(page)).toBeEmpty();
	expect(await Promise.all([storedFavorites(page, retired), storedFavorites(page, 'replacement-context')])).toEqual([null, null]);
	await page.evaluate((key) => window.dispatchEvent(new StorageEvent('storage', { key })), transitionKey);
	await expect(favorite(page)).toHaveCount(0);
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
});

test('when persistence refuses, the status describes a visit-only membership change', async ({ page }) => {
	await openLibrary(page);
	await page.evaluate((prefix) => {
		const setItem = Storage.prototype.setItem;
		Storage.prototype.setItem = function (key, value) {
			if (this === localStorage && key.startsWith(`${prefix}.`)) throw new DOMException('Refused', 'QuotaExceededError');
			setItem.call(this, key, value);
		};
	}, favoritesBase);
	await favorite(page).click();
	await expectMembership(page, tracks[0].id, 'Starred North Wind for this visit.', true);
});
