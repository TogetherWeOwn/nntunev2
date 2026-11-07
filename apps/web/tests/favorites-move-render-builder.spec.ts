import { expect, test, type Page } from '@playwright/test';

const ITEMS = [
	{ id: 'move-a', title: 'Alpha', artist: 'First artist' },
	{ id: 'move-b', title: 'Beta', artist: 'Second artist' },
	{ id: 'move-c', title: 'Coral', artist: 'Third artist' }
];
const CONTEXT = 'move-render-account';
const KEY = `nntune.library.favorites.v1.${CONTEXT}`;

const rows = (page: Page) => page.getByTestId('favorite-item');
const move = (page: Page, direction: 'up' | 'down') =>
	page.getByTestId('favorites-section').getByRole('button', {
		name: `Move favorite Beta ${direction}`, exact: true
	});

for (const theme of ['light', 'dark'] as const) {
	for (const width of [320, 1280]) {
		test(`Favorites move rendering retains both native controls through boundary moves ${theme} ${width}`, async ({ page }) => {
			const errors: string[] = [];
			const mutations: string[] = [];
			page.on('pageerror', (error) => errors.push(error.message));
			page.on('request', (request) => {
				if (request.method() !== 'GET') mutations.push(request.method());
			});
			await page.context().route('**/*', (route) => {
				const url = new URL(route.request().url());
				if (!['localhost', '127.0.0.1'].includes(url.hostname) || /^\/(?:api|v1)(?:\/|$)/.test(url.pathname)) {
					return route.abort('blockedbyclient');
				}
				return route.continue();
			});
			await page.route('**/api/library', (route) => route.fulfill({ json: ITEMS }));
			await page.addInitScript(({ context, key, ids }) => {
				sessionStorage.setItem('nntune.library.auth-context.v1', context);
				localStorage.setItem(key, JSON.stringify(ids));
			}, { context: CONTEXT, key: KEY, ids: ITEMS.map((item) => item.id) });
			await page.setViewportSize({ width, height: 900 });
			await page.goto('/library');
			await expect(rows(page)).toHaveCount(3);
			await page.getByRole('combobox', { name: 'Appearance', exact: true }).selectOption(theme);

			await expect(rows(page).locator('.nt-favorite-move')).toHaveCount(6);
			expect(await rows(page).locator('.nt-favorite-move').evaluateAll((buttons) => buttons.map((node) => ({
				text: node.textContent?.trim(), label: node.getAttribute('aria-label'),
				disabled: (node as HTMLButtonElement).disabled
			})))).toEqual(ITEMS.flatMap((item, index) => [
				{ text: 'Up', label: `Move favorite ${item.title} up`, disabled: index === 0 },
				{ text: 'Down', label: `Move favorite ${item.title} down`, disabled: index === 2 }
			]));
			const up = (await move(page, 'up').elementHandle())!;
			const down = (await move(page, 'down').elementHandle())!;
			await move(page, 'up').focus();
			await page.keyboard.press('Enter');
			await expect.poll(() => rows(page).evaluateAll((nodes) => nodes.map((node) => node.getAttribute('data-id'))))
				.toEqual(['move-b', 'move-a', 'move-c']);
			await expect(move(page, 'up')).toBeDisabled();
			await expect(move(page, 'down')).toBeFocused();
			await expect(page.getByTestId('favorites-announcement')).toHaveText('Beta moved to 1 of 3');

			await page.keyboard.press('Enter');
			await expect.poll(() => rows(page).evaluateAll((nodes) => nodes.map((node) => node.getAttribute('data-id'))))
				.toEqual(['move-a', 'move-b', 'move-c']);
			await expect(move(page, 'down')).toBeFocused();
			await expect(move(page, 'up')).toBeEnabled();
			await expect(page.getByTestId('favorites-announcement')).toHaveText('Beta moved to 2 of 3');

			await page.keyboard.press('Enter');
			await expect.poll(() => rows(page).evaluateAll((nodes) => nodes.map((node) => node.getAttribute('data-id'))))
				.toEqual(['move-a', 'move-c', 'move-b']);
			await expect(move(page, 'down')).toBeDisabled();
			await expect(move(page, 'up')).toBeFocused();
			await expect(page.getByTestId('favorites-announcement')).toHaveText('Beta moved to 3 of 3');
			expect(await up.evaluate((node) => node === document.querySelector('[aria-label="Move favorite Beta up"]'))).toBe(true);
			expect(await down.evaluate((node) => node === document.querySelector('[aria-label="Move favorite Beta down"]'))).toBe(true);
			expect(await page.evaluate((key) => JSON.parse(localStorage.getItem(key) ?? 'null'), KEY))
				.toEqual(['move-a', 'move-c', 'move-b']);
			await expect(page.locator('audio')).toHaveCount(0);
			expect(errors).toEqual([]);
			expect(mutations).toEqual([]);
		});
	}
}
