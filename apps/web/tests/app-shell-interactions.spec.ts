import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * App-shell interaction acceptance: keyboard traversal of the Account
 * menu, and the per-toast Undo action on the Liked page.
 *
 * This file lives next to the protected NAV contract
 * (app-shell-navigation.spec.ts, Builder-may-not-edit): new interaction
 * coverage lands here so the NAV pins stay untouched. No test imports
 * project code; every locator names rendered UI and every failure message
 * states the expected UI and the Builder's next action.
 *
 * Run focused (from apps/web):
 *   pnpm exec playwright test app-shell-interactions
 */

const PRESENCE_TIMEOUT = 3000;
const SHELL_BUILDER = 'Builder (app shell): see the v2 contract in app-shell-navigation.spec.ts.';

const LIBRARY_ITEMS = [
	{ id: 'nav-track-1', title: 'Navigation Song One', artist: 'Shell Band' },
	{ id: 'nav-track-2', title: 'Navigation Song Two', artist: 'Shell Band' }
];

// Device-local favorites are scoped by the session auth context; seeding a
// fixed context plus its favorites key renders the Liked page populated
// without touching project code.
const FAVORITES_CONTEXT = 'shell-interactions';
const FAVORITES_KEY = `nntune.library.favorites.v1.${FAVORITES_CONTEXT}`;

async function mockLibrary(page: Page, status: number, body: unknown): Promise<void> {
	await page.route('**/api/library', (route) =>
		route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })
	);
}

async function seedFavorites(page: Page, ids: string[]): Promise<void> {
	await page.addInitScript(
		({ key, context, favoriteIds }) => {
			sessionStorage.setItem('nntune.library.auth-context.v1', context);
			localStorage.setItem(key, JSON.stringify(favoriteIds));
		},
		{ key: FAVORITES_KEY, context: FAVORITES_CONTEXT, favoriteIds: ids }
	);
}

function accountMenuButton(page: Page): Locator {
	return page.getByRole('button', { name: 'Account menu', exact: true });
}

// [gate: app shell]
test('account menu supports arrow-key traversal with focus return', async ({ page }) => {
	await mockLibrary(page, 200, []);
	await page.goto('/library', { waitUntil: 'load' });
	await expect(
		page.getByRole('navigation', { name: 'Primary', exact: true }),
		`expected the app-shell Primary nav on /library. ${SHELL_BUILDER}`
	).toHaveCount(1, { timeout: PRESENCE_TIMEOUT });

	// Opening the menu moves focus from the trigger to the first item.
	await accountMenuButton(page).click();
	const menu = page.locator('#nt-account-menu');
	await expect(menu, `expected the account menu to open. ${SHELL_BUILDER}`).toBeVisible();
	const items = menu.getByRole('menuitem');
	await expect(items.first(), 'expected focus to move to the first menu item on open').toBeFocused();

	// ArrowDown and ArrowUp travel within the menu, wrapping at the ends.
	await page.keyboard.press('ArrowDown');
	await expect(items.nth(1), 'expected ArrowDown to move focus to the second menu item').toBeFocused();
	await page.keyboard.press('ArrowDown');
	await expect(items.nth(2), 'expected ArrowDown to move focus to the third menu item').toBeFocused();
	await page.keyboard.press('ArrowDown');
	await expect(
		items.first(),
		'expected ArrowDown on the last item to wrap to the first menu item'
	).toBeFocused();
	await page.keyboard.press('ArrowUp');
	await expect(
		items.nth(2),
		'expected ArrowUp on the first item to wrap to the last menu item'
	).toBeFocused();

	// Escape closes the menu and returns focus to the trigger.
	await page.keyboard.press('Escape');
	await expect(menu, `expected Escape to close the account menu. ${SHELL_BUILDER}`).toHaveCount(0);
	await expect(
		accountMenuButton(page),
		'expected Escape to return focus to the account-menu trigger'
	).toBeFocused();
});

// [gate: app shell]
test('Liked Undo restores only its own track and dismisses only its toast', async ({
	page
}) => {
	await mockLibrary(page, 200, LIBRARY_ITEMS);
	await seedFavorites(page, ['nav-track-1', 'nav-track-2']);
	await page.goto('/library/liked', { waitUntil: 'load' });

	const removeOne = page.getByRole('button', { name: 'Remove Navigation Song One from Liked' });
	const removeTwo = page.getByRole('button', { name: 'Remove Navigation Song Two from Liked' });
	await expect(
		removeOne,
		`expected a populated Liked list with a remove control per track. ${SHELL_BUILDER}`
	).toBeVisible();
	await expect(removeTwo).toBeVisible();

	await removeOne.click();
	const toastOne = page.getByTestId('toast').filter({ hasText: 'Navigation Song One' });
	await expect(
		toastOne,
		'expected the first removal to raise its own Undo toast'
	).toBeVisible({ timeout: PRESENCE_TIMEOUT });
	await removeTwo.click();
	const toastTwo = page.getByTestId('toast').filter({ hasText: 'Navigation Song Two' });
	await expect(toastTwo, 'expected the second removal to raise its own Undo toast').toBeVisible({
		timeout: PRESENCE_TIMEOUT
	});

	// Undo runs the toast action, then dismisses only the clicked toast.
	await toastOne.getByRole('button', { name: 'Undo' }).click();
	await expect(
		removeOne,
		'expected Undo to restore only the first removed track to the Liked list'
	).toBeVisible({ timeout: PRESENCE_TIMEOUT });
	await expect(toastOne, 'expected Undo to dismiss only its own toast').toHaveCount(0);
	await expect(
		toastTwo,
		'expected the second toast to survive the first Undo within the 8s window'
	).toBeVisible();
	await expect(
		removeTwo,
		'expected the second track to stay removed after the first Undo'
	).toHaveCount(0);

	await toastTwo.getByRole('button', { name: 'Undo' }).click();
	await expect(removeTwo, 'expected the second Undo to restore the second track').toBeVisible({
		timeout: PRESENCE_TIMEOUT
	});
	await expect(
		page.getByTestId('toast'),
		'expected both toasts dismissed after both Undos'
	).toHaveCount(0);
});
