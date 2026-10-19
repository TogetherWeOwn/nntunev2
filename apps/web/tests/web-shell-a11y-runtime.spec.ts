import { expect, test } from '@playwright/test';

for (const viewport of [{ width: 1280, height: 800 }, { width: 390, height: 844 }]) {
	test.describe(`${viewport.width}px web shell`, () => {
		test.use({ viewport });

		test('account navigation moves the current-page marker after client navigation', async ({ page }, testInfo) => {
			await page.route('**/v1/vault', (route) =>
				route.fulfill({ status: 200, contentType: 'application/json', body: '{}' })
			);
			await page.route('**/v1/me/*', (route) =>
				route.fulfill({ status: 503, contentType: 'application/json', body: '{}' })
			);
			await page.goto('/account/security');
			const nav = page.getByRole('navigation', { name: 'Account', exact: true });
			await expect(nav.getByRole('link', { name: 'Security' })).toHaveAttribute('aria-current', 'page');
			await expect(nav.locator('[aria-current="page"]')).toHaveCount(1);
			await page.screenshot({ path: testInfo.outputPath('account-security.png'), fullPage: true });

			await nav.getByRole('link', { name: 'Billing' }).click();
			await expect(nav.getByRole('link', { name: 'Billing' })).toHaveAttribute('aria-current', 'page');
			await expect(nav.getByRole('link', { name: 'Security' })).not.toHaveAttribute('aria-current', 'page');
			await expect(nav.locator('[aria-current="page"]')).toHaveCount(1);
			await page.screenshot({ path: testInfo.outputPath('account-billing.png'), fullPage: true });
		});

		test('queue announcements stay unchanged on append and update on Next', async ({ page }, testInfo) => {
			await page.route('**/api/library', (route) =>
				route.fulfill({
					status: 200,
					contentType: 'application/json',
					body: JSON.stringify([
						{ id: 'mango', title: 'Mango Season', artist: 'Acceptance Band' },
						{ id: 'apple', title: 'Apple Cart', artist: 'Acceptance Band' }
					])
				})
			);
			await page.goto('/library');
			const nowPlaying = page.getByTestId('now-playing');
			await expect(nowPlaying).toContainText('Queue is empty');
			await expect(page.getByRole('list', { name: 'Playback queue' })).toBeAttached();
			await page.screenshot({ path: testInfo.outputPath('queue-empty.png'), fullPage: true });
			const mangoButton = page.getByTestId('library-item').filter({ hasText: 'Mango Season' }).getByRole('button');
			await expect(mangoButton).toHaveAccessibleName('Mango Season — Acceptance Band');
			await expect(mangoButton).not.toHaveAttribute('aria-pressed');
			await mangoButton.click();
			await expect(mangoButton).toHaveAccessibleName('Mango Season — Acceptance Band — Queued');
			await expect(mangoButton).not.toHaveAttribute('aria-pressed');
			await mangoButton.click();
			await expect(page.getByTestId('queue-item')).toHaveCount(1);
			await expect(mangoButton).toHaveAccessibleName('Mango Season — Acceptance Band — Queued');
			await expect(nowPlaying).toContainText('Mango Season');
			await expect(page.getByRole('list', { name: 'Playback queue' })).toBeVisible();
			await nowPlaying.evaluate((element) => {
				const observed = element as HTMLElement & { mutations: MutationRecord[]; observer: MutationObserver };
				observed.mutations = [];
				observed.observer = new MutationObserver((records) => observed.mutations.push(...records));
				observed.observer.observe(element, { childList: true, characterData: true, subtree: true });
			});

			await page.getByRole('button', { name: 'Apple Cart — Acceptance Band' }).click();
			await expect(page.getByTestId('queue-item')).toHaveCount(2);
			expect(await nowPlaying.evaluate((element) => {
				const observed = element as HTMLElement & { mutations: MutationRecord[] };
				return observed.mutations.length;
			})).toBe(0);
			await page.screenshot({ path: testInfo.outputPath('queue-appended.png'), fullPage: true });

			await page.getByTestId('player-next').click();
			await expect(nowPlaying).toContainText('Apple Cart');
			expect(await nowPlaying.evaluate((element) => {
				const observed = element as HTMLElement & { mutations: MutationRecord[]; observer: MutationObserver };
				observed.observer.disconnect();
				return observed.mutations.length;
			})).toBeGreaterThan(0);
			await expect(mangoButton).toHaveAccessibleName('Mango Season — Acceptance Band — Queued');
			await expect(mangoButton).not.toHaveAttribute('aria-pressed');
			await page.screenshot({ path: testInfo.outputPath('queue-advanced.png'), fullPage: true });
		});

		test('404 keeps the existing recovery navigation', async ({ page }, testInfo) => {
			await page.goto('/missing-web-shell-page');
			await expect(page.getByRole('heading', { level: 1 })).toContainText('404');
			await expect(page.getByRole('button', { name: 'Reload' })).toHaveCount(0);
			await page.screenshot({ path: testInfo.outputPath('error-404.png'), fullPage: true });
		});
	});
}
