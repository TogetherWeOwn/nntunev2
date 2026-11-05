import { expect, test, type Page } from '@playwright/test';

const CONTEXT = 'shell-undo-review';
const FAVORITES_KEY = `nntune.library.favorites.v1.${CONTEXT}`;
const RECENT_KEY = `nntune.library.recent-finds.v1.${CONTEXT}`;
const ITEMS = [
	{ id: 'A', title: 'Track A' },
	{ id: 'B', title: 'Track B' },
	{ id: 'C', title: 'Track C' }
];

async function openLiked(page: Page): Promise<void> {
	await page.route('**/api/library', (route) =>
		route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(ITEMS) })
	);
	await page.addInitScript(({ context, key }) => {
		sessionStorage.setItem('nntune.library.auth-context.v1', context);
		localStorage.setItem(key, JSON.stringify(['A', 'B']));
	}, { context: CONTEXT, key: FAVORITES_KEY });
	await page.goto('/library/liked');
	await expect(page.getByRole('button', { name: 'Remove Track A from Liked' })).toBeVisible();
}

for (const current of [['B', 'C'], []]) {
	test(`stale Liked removal preserves the current stored list ${JSON.stringify(current)}`, async ({ page }) => {
		await openLiked(page);
		// Model a tab sharing the auth context changing storage after render.
		await page.evaluate(({ key, ids }) => localStorage.setItem(key, JSON.stringify(ids)), {
			key: FAVORITES_KEY, ids: current
		});
		await page.getByRole('button', { name: 'Remove Track A from Liked' }).click();
		await expect(page.getByRole('button', { name: 'Remove Track A from Liked' })).toHaveCount(0);
		expect(await page.evaluate((key) => JSON.parse(localStorage.getItem(key)!), FAVORITES_KEY)).toEqual(current);
	});
}

test('Liked Undo restores memory when writes fail, without undoing another removal', async ({ page }) => {
	await openLiked(page);
	await page.evaluate((key) => {
		const original = Storage.prototype.setItem;
		Storage.prototype.setItem = function (name, value) {
			if (name === key) throw new DOMException('Full', 'QuotaExceededError');
			original.call(this, name, value);
		};
	}, FAVORITES_KEY);
	const removeA = page.getByRole('button', { name: 'Remove Track A from Liked' });
	const removeB = page.getByRole('button', { name: 'Remove Track B from Liked' });
	await removeA.click();
	await removeB.click();
	await page.getByTestId('toast').filter({ hasText: 'Track A' }).getByRole('button', { name: 'Undo' }).click();
	await expect(removeA).toBeVisible();
	await expect(removeB).toHaveCount(0);
	await page.getByTestId('toast').filter({ hasText: 'Track B' }).getByRole('button', { name: 'Undo' }).click();
	await expect(removeB).toBeVisible();
});

test('Liked Undo merges another tab’s additions after failed writes recover', async ({ page }) => {
	await openLiked(page);
	await page.evaluate((key) => {
		const original = Storage.prototype.setItem;
		Storage.prototype.setItem = function (name, value) {
			if (name === key) throw new DOMException('Full', 'QuotaExceededError');
			original.call(this, name, value);
		};
		Object.assign(window, { restoreWrites: () => { Storage.prototype.setItem = original; } });
	}, FAVORITES_KEY);
	await page.getByRole('button', { name: 'Remove Track A from Liked' }).click();
	await page.getByRole('button', { name: 'Remove Track B from Liked' }).click();
	await page.evaluate((key) => {
		(window as unknown as { restoreWrites: () => void }).restoreWrites();
		localStorage.setItem(key, JSON.stringify(['A', 'B', 'C']));
	}, FAVORITES_KEY);
	await page.getByTestId('toast').filter({ hasText: 'Track A' }).getByRole('button', { name: 'Undo' }).click();
	await expect(page.getByRole('button', { name: 'Remove Track A from Liked' })).toBeVisible();
	await expect(page.getByRole('button', { name: 'Remove Track B from Liked' })).toHaveCount(0);
	await expect(page.getByRole('button', { name: 'Remove Track C from Liked' })).toBeVisible();
	expect(await page.evaluate((key) => JSON.parse(localStorage.getItem(key)!), FAVORITES_KEY)).toEqual(['A', 'C']);
});

test('Recent Undo preserves the new recency of a repeated search', async ({ page }) => {
	await page.addInitScript(({ context, key }) => {
		sessionStorage.setItem('nntune.library.auth-context.v1', context);
		localStorage.setItem(key, JSON.stringify(['A', 'B']));
	}, { context: CONTEXT, key: RECENT_KEY });
	await page.goto('/library/recent');
	await page.getByRole('button', { name: 'Clear recent searches', exact: true }).click();
	await page.evaluate((key) => localStorage.setItem(key, JSON.stringify(['B'])), RECENT_KEY);
	await page.getByTestId('toast').getByRole('button', { name: 'Undo' }).click();
	await expect(page.getByRole('main').locator('li a')).toHaveText(['B', 'A']);
	expect(await page.evaluate((key) => JSON.parse(localStorage.getItem(key)!), RECENT_KEY)).toEqual(['B', 'A']);
});
