import { expect, test } from '@playwright/test';
import { openQueue } from './helpers/queue-host.js';

for (const colorScheme of ['light', 'dark'] as const) {
	for (const width of [320, 390]) {
		test(`mobile content clears the sticky player and navigation: ${colorScheme} ${width}`, async ({ browser }) => {
			const context = await browser.newContext({ viewport: { width, height: 1000 }, isMobile: true, hasTouch: true, colorScheme });
			const page = await context.newPage();
			const errors: string[] = [];
			page.on('pageerror', (error) => errors.push(error.message));
			const items = [
				{ id: 'coral', title: 'Coral Night', artist: 'Harbor Lights' },
				{ id: 'dawn', title: 'Dawn Chorus' },
				{ id: 'ember', title: 'Ember Sky' },
				{ id: 'tide', title: 'Tide Pools' }
			];
			await page.route('**/api/library', (route) => route.fulfill({ json: items }));
			await page.goto('/library');
			await expect(page.getByTestId('library-item')).toHaveCount(items.length);
			await expect(page.getByTestId('mini-player')).toHaveCount(0);
			const clearance = () => page.locator('.nt-body').evaluate((node) => ({
				padding: parseFloat(getComputedStyle(node).paddingBottom),
				nav: document.querySelector('[data-testid="shell-tabbar"]')!.getBoundingClientRect().height,
				player: document.querySelector('[data-testid="mini-player"]')?.getBoundingClientRect().height ?? 0,
				gap: 8
			}));
			const empty = await clearance();
			expect(empty.padding).toBeGreaterThanOrEqual(empty.nav + empty.gap);
			await page.getByRole('button', { name: 'Add displayed tracks to queue', exact: true }).click();
			await openQueue(page);
			await expect(page.getByTestId('mini-player')).toBeVisible();
			const loaded = await clearance();
			expect(loaded.padding).toBeGreaterThanOrEqual(loaded.nav + loaded.player + loaded.gap);
			expect(loaded.padding).toBeGreaterThan(empty.padding);
			const grip = page.getByTestId('queue-panel').getByRole('button', { name: 'Reorder Ember Sky', exact: true });
			await grip.scrollIntoViewIfNeeded();
			expect(await grip.evaluate((node) => {
				const rect = node.getBoundingClientRect();
				return node.contains(document.elementFromPoint(rect.x + 8, rect.y + 8));
			})).toBe(true);
			// Start a distinct native-focus check with its target below the viewport.
			const clear = page.getByTestId('queue-panel').getByRole('button', { name: 'Clear queue', exact: true });
			await page.evaluate(() => window.scrollTo(0, 0));
			expect(await clear.evaluate((node) => node.getBoundingClientRect().top > window.innerHeight)).toBe(true);
			await clear.focus();
			await expect(clear).toBeFocused();
			expect(await clear.evaluate((node) => {
				const rect = node.getBoundingClientRect();
				const player = document.querySelector('[data-testid="mini-player"]')!.getBoundingClientRect();
				return rect.bottom + 8 <= player.top && node.contains(document.elementFromPoint(rect.x + rect.width / 2, rect.y + rect.height / 2));
			})).toBe(true);
			await expect(page.locator('audio[src]:not([src=""])')).toHaveCount(0);
			expect(await page.getByTestId('queue-item').evaluateAll((rows) => rows.map((row) => row.getAttribute('data-track-id')))).toEqual(items.map((item) => item.id));
			expect(errors).toEqual([]);
			await context.close();
		});
	}
}
