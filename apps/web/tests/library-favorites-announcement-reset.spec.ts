import { expect, test, type Locator, type Page } from '@playwright/test';

const ITEMS = [
	{ id: 'announce-z', title: 'Zulu Song', artist: 'Harbor Lights' },
	{ id: 'announce-a', title: 'Alpha Song', artist: 'Cedar' },
	{ id: 'announce-m', title: 'Mike Song', artist: 'Harbor Lights' }
];
const CONTEXT_KEY = 'nntune.library.auth-context.v1';
const KEY = 'nntune.library.favorites.v1.announcement-reset-account';

function favorites(page: Page): Locator {
	return page.getByTestId('favorites-section');
}

function announcement(page: Page): Locator {
	return favorites(page).getByTestId('favorites-announcement');
}

function move(page: Page, title: string, direction: 'up' | 'down'): Locator {
	return favorites(page).getByRole('button', { name: `Move favorite ${title} ${direction}`, exact: true });
}

function star(page: Page, id: string): Locator {
	return page
		.locator(`[data-testid="library-item"][data-id="${id}"]`)
		.getByRole('button', { name: /^(?:Unstar|Star) / });
}

async function open(page: Page, favoriteIds: string[]): Promise<void> {
	await page.addInitScript(({ contextKey, key, favoriteIds }) => {
		sessionStorage.setItem(contextKey, 'announcement-reset-account');
		localStorage.setItem(key, JSON.stringify(favoriteIds));
	}, { contextKey: CONTEXT_KEY, key: KEY, favoriteIds });
	await page.route('**/api/library', (route) =>
		route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(ITEMS) })
	);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(favorites(page)).toBeVisible();
}

test('clears a prior favorite-move announcement when a favorite is unstarred', async ({ page }) => {
	await open(page, ['announce-z', 'announce-a', 'announce-m']);
	await move(page, 'Mike Song', 'up').click();
	await expect(announcement(page)).toHaveText('Mike Song moved to 2 of 3');

	await star(page, 'announce-z').click();

	await expect(announcement(page)).toHaveText('');
});

test('clears a prior favorite-move announcement when a track is starred', async ({ page }) => {
	await open(page, ['announce-z', 'announce-a']);
	await move(page, 'Alpha Song', 'up').click();
	await expect(announcement(page)).toHaveText('Alpha Song moved to 1 of 2');

	await star(page, 'announce-m').click();

	await expect(announcement(page)).toHaveText('');
});
