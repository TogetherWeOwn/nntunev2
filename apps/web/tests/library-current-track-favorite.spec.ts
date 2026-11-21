import { expect, test, type Page } from '@playwright/test';

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
	await expect(page.getByTestId('library-item')).toHaveCount(2);
	await expect(favorite(page)).toHaveCount(0);
	for (const { id, title } of tracks) await row(page, id).getByRole('button', { name: title, exact: true }).click();
	await expect(favorite(page)).toHaveAccessibleName('Favorite North Wind');
}

async function storedFavorites(page: Page) {
	return page.evaluate(({ contextKey, favoritesBase }) =>
		localStorage.getItem(`${favoritesBase}.${sessionStorage.getItem(contextKey)}`), { contextKey, favoritesBase });
}

test('the player stars the selected track once and shares membership with Library and queue controls', async ({ page }) => {
	await openLibrary(page);
	const [north, south] = tracks;
	await queueRow(page, south.id).getByRole('button', { name: 'Select South Wind', exact: true }).click();
	await expect(page.getByTestId('now-playing')).toContainText('South Wind');
	await expect(page.getByTestId('player-next')).toBeEnabled();
	await expect(favorite(page)).toHaveAccessibleName('Favorite South Wind');
	await expect(favorite(page)).toHaveAttribute('aria-pressed', 'false');
	await favorite(page).focus();
	await page.keyboard.press('Space');
	await expect(favorite(page)).toBeFocused();
	await expect(favorite(page)).toHaveAttribute('aria-pressed', 'true');
	await expect(row(page, north.id).getByRole('button', { name: 'Star North Wind', exact: true })).toHaveAttribute('aria-pressed', 'false');
	await expect(row(page, south.id).getByRole('button', { name: 'Unstar South Wind', exact: true })).toHaveAttribute('aria-pressed', 'true');
	await expect(feedback(page)).toHaveText('Starred South Wind.');
	await expect(page.getByRole('status').filter({ hasText: 'Starred South Wind.' })).toHaveCount(1);
	await expect(page.getByTestId('audio-player')).toHaveCount(1);
	expect(await storedFavorites(page)).toBe(JSON.stringify([south.id]));

	await queueRow(page, south.id).getByRole('button', { name: 'Unstar South Wind', exact: true }).click();
	await expect(favorite(page)).toHaveAttribute('aria-pressed', 'false');
	await expect(feedback(page)).toHaveText('');
	await row(page, south.id).getByRole('button', { name: 'Star South Wind', exact: true }).click();
	await expect(favorite(page)).toHaveAttribute('aria-pressed', 'true');
	await expect(feedback(page)).toHaveText('');
});

test('a click on a retired selection cannot star either track', async ({ page }) => {
	await openLibrary(page);
	const selectSouth = queueRow(page, tracks[1].id).getByRole('button', { name: 'Select South Wind', exact: true });
	await selectSouth.evaluate((button) => { button.id = 'select-south-before-favorite'; });
	await favorite(page).evaluate((button) => {
		button.addEventListener('click', () => document.getElementById('select-south-before-favorite')?.click(),
			{ capture: true, once: true });
	});
	await favorite(page).click();
	await expect(favorite(page)).toHaveAccessibleName('Favorite South Wind');
	await expect(favorite(page)).toHaveAttribute('aria-pressed', 'false');
	await expect(feedback(page)).toHaveText('');
	expect(await storedFavorites(page)).toBeNull();
});

test('a rotated account refuses the stale button and an auth transition retires the control', async ({ page }) => {
	await openLibrary(page);
	await favorite(page).evaluate((button, key) => {
		button.addEventListener('click', () => sessionStorage.setItem(key, 'replacement-context'),
			{ capture: true, once: true });
	}, contextKey);
	await favorite(page).click();
	await expect(feedback(page)).toHaveText('');
	expect(await storedFavorites(page)).toBeNull();
	await page.evaluate((key) => window.dispatchEvent(new StorageEvent('storage', { key })), transitionKey);
	await expect(favorite(page)).toHaveCount(0);
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
});

test('leaving the Library owner retires the action without replacing the decoder', async ({ page }) => {
	await openLibrary(page);
	await page.getByTestId('audio-player').evaluate((audio) => { audio.id = 'favorite-decoder'; });
	await page.getByRole('link', { name: 'Search', exact: true }).first().click();
	await expect(page).toHaveURL(/\/search$/);
	await expect(favorite(page)).toHaveCount(0);
	await expect(page.getByTestId('audio-player')).toHaveAttribute('id', 'favorite-decoder');
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
	await expect(favorite(page)).toHaveAttribute('aria-pressed', 'true');
	await expect(feedback(page)).toHaveText('Starred North Wind for this visit.');
	expect(await storedFavorites(page)).toBeNull();
});
