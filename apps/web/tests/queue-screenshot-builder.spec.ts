import { expect, test } from '@playwright/test';

for (const colorScheme of ['light', 'dark'] as const) {
	test(`full-page capture preserves the desktop queue host and management focus in ${colorScheme}`, async ({ page }) => {
		await page.setViewportSize({ width: 1280, height: 844 });
		await page.emulateMedia({ colorScheme });
		await page.route('**/api/library', (route) => route.fulfill({ json: [
			{ id: 'capture-a', title: 'Capture A' }, { id: 'capture-b', title: 'Capture B' },
			{ id: 'capture-c', title: 'Capture C' }, { id: 'capture-d', title: 'Capture D' }
		] }));
		await page.addInitScript(() => {
			sessionStorage.setItem('nntune.library.auth-context.v1', 'capture-account');
			localStorage.setItem('nntune.library.favorites.v1.capture-account', '["capture-a","capture-b"]');
			const events: unknown[] = [];
			Object.assign(window, { captureEvents: events });
			const media = matchMedia('(max-width: 799px)');
			media.addEventListener('change', () => events.push({ kind: 'media', matches: media.matches, width: innerWidth, url: location.href }));
			addEventListener('resize', () => events.push({ kind: 'resize', width: innerWidth, url: location.href }));
		});
		await page.goto('/library');
		await expect(page.getByTestId('library-item')).toHaveCount(4);
		for (const id of ['capture-a', 'capture-b', 'capture-c']) {
			await page.locator(`[data-testid="library-item"][data-id="${id}"]`).click();
		}
		await page.getByTestId('player-queue-toggle').click();
		await expect(page.getByTestId('queue-panel')).toBeVisible();
		await page.getByRole('button', { name: 'Clear local favorites', exact: true }).click();
		const confirm = page.getByRole('button', { name: 'Yes, clear local favorites', exact: true });
		await confirm.focus();
		try {
			for (let capture = 0; capture < 4; capture += 1) {
				await page.screenshot({ fullPage: true });
				await expect(page).toHaveURL(/\/library$/);
				await expect(confirm).toBeFocused();
				await expect(page.getByTestId('queue-panel')).toBeVisible();
			}
			await page.keyboard.press('Enter');
			await expect(page.getByTestId('favorite-item')).toHaveCount(0);
			await expect(page).toHaveURL(/\/library$/);
		} finally {
			await test.info().attach('capture-events', { body: JSON.stringify(await page.evaluate(() => Reflect.get(window, 'captureEvents'))), contentType: 'application/json' });
		}
	});
}
