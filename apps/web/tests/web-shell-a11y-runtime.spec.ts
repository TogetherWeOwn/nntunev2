import { expect, test } from '@playwright/test';
import { test as observedTest, expectMobileQueue, observedQueue, returnToMobileLibrary, metadataManagementState, installManagementMediaProbe } from './helpers/favorites-mobile';
import { openQueue } from './helpers/queue-host';

for (const viewport of [{ width: 1280, height: 800 }, { width: 390, height: 844 }]) {
	test.describe(`${viewport.width}px web shell`, () => {
		test.use({ viewport });

		test('account navigation moves the current-page marker after client navigation', async ({ page }, testInfo) => {
			await page.route('**/v1/me/*', (route) =>
				route.fulfill({ status: 503, contentType: 'application/json', body: '{}' })
			);
			await page.route('**/v1/me/subscription', (route) =>
				route.fulfill({
					status: 200,
					contentType: 'application/json',
					body: JSON.stringify({
						state: 'no_subscription',
						planId: null,
						status: null,
						currentPeriodEnd: null,
						graceUntil: null,
						cancelAtPeriodEnd: false
					})
				})
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

			await nav.getByRole('link', { name: 'Security' }).click();
			await expect(nav.getByRole('link', { name: 'Security' })).toHaveAttribute('aria-current', 'page');
			await expect(nav.getByRole('link', { name: 'Billing' })).not.toHaveAttribute('aria-current', 'page');
			await expect(nav.locator('[aria-current="page"]')).toHaveCount(1);
			await expect(page.getByRole('heading', { name: 'Account security' })).toBeVisible();
			await page.screenshot({ path: testInfo.outputPath('account-security-again.png'), fullPage: true });
		});

		if (viewport.width === 1280) test('queue announcements stay unchanged on append and update on Next', async ({ page }, testInfo) => {
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
			await openQueue(page);
			const nowPlaying = page.getByTestId('now-playing');
			await expect(nowPlaying).toContainText('Queue is empty');
			await expect(page.getByRole('list', { name: 'Playback queue' })).toBeAttached();
			await page.screenshot({ path: testInfo.outputPath('queue-empty.png'), fullPage: true });
			// Queue control only: the row also carries a star toggle button.
			const mangoButton = page.getByTestId('library-item').filter({ hasText: 'Mango Season' }).locator('button.nt-library-row');
			await expect(mangoButton).toHaveAccessibleName('Mango Season — Acceptance Band');
			await expect(mangoButton).not.toHaveAttribute('aria-pressed');
			await mangoButton.click();
			await expect(mangoButton).toHaveAccessibleName('Mango Season — Acceptance Band — Queued');
			await expect(mangoButton).toHaveAttribute('aria-pressed', 'true');
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
			await expect(page.getByTestId('queue-panel')).toHaveCount(1);
			expect(await nowPlaying.evaluate((element) => {
				const observed = element as HTMLElement & { mutations: MutationRecord[] };
				return observed.mutations.length;
			})).toBe(0);
			await page.screenshot({ path: testInfo.outputPath('queue-appended.png'), fullPage: true });

			await page.getByTestId('player-next').click();
			await expect(nowPlaying).toContainText('Apple Cart');
			await expect(page.getByTestId('queue-panel')).toHaveCount(1);
			expect(await nowPlaying.evaluate((element) => {
				const observed = element as HTMLElement & { mutations: MutationRecord[]; observer: MutationObserver };
				observed.observer.disconnect();
				return observed.mutations.length;
			})).toBeGreaterThan(0);
			await expect(mangoButton).toHaveAccessibleName('Mango Season — Acceptance Band — Queued');
			await expect(mangoButton).toHaveAttribute('aria-pressed', 'true');
			await page.screenshot({ path: testInfo.outputPath('queue-advanced.png'), fullPage: true });
		});
		else observedTest('queue announcements stay unchanged on append and update on Next', async ({ page, observeMobileLibrary }, testInfo) => {
			const requests: { url: string; type: string }[] = [];
			page.on('request', (request) => requests.push({ url: request.url(), type: request.resourceType() }));
			let libraryHits = 0;
			let libraryMounts = 0;
			await installManagementMediaProbe(page);
			await page.route('**/api/library', (route) => {
				libraryHits += 1;
				return route.fulfill({
					status: 200,
					contentType: 'application/json',
					body: JSON.stringify([
						{ id: 'mango', title: 'Mango Season', artist: 'Acceptance Band' },
						{ id: 'apple', title: 'Apple Cart', artist: 'Acceptance Band' }
					])
				});
			});
			await page.goto(await observeMobileLibrary());
			libraryMounts += 1;
			await openQueue(page);
			const nowPlaying = page.getByTestId('now-playing');
			await expect(nowPlaying).toContainText('Queue is empty');
			await expect(page.getByRole('list', { name: 'Playback queue' })).toBeAttached();
			expect(await observedQueue(page)).toEqual({ ids: [], currentIndex: -1, currentId: null, selectedTitle: null });
			const emptyVersion = await page.evaluate(() => window.__favoritesQueueOwner?.selectionVersion);
			expect(emptyVersion).toBe(0);
			await page.screenshot({ path: testInfo.outputPath('queue-empty.png'), fullPage: true });
			await returnToMobileLibrary(page);
			libraryMounts += 1;
			const mangoButton = page.getByTestId('library-item').filter({ hasText: 'Mango Season' }).locator('button.nt-library-row');
			await expect(mangoButton).toHaveAccessibleName('Mango Season — Acceptance Band');
			await expect(mangoButton).not.toHaveAttribute('aria-pressed');
			await mangoButton.click();
			await expect(mangoButton).toHaveAccessibleName('Mango Season — Acceptance Band — Queued');
			await expect(mangoButton).toHaveAttribute('aria-pressed', 'true');
			await mangoButton.click();
			await expect(mangoButton).toHaveAccessibleName('Mango Season — Acceptance Band — Queued');
			const mango = { ids: ['mango'], currentIndex: 0, currentId: 'mango', selectedTitle: 'Mango Season' };
			const owner = await expectMobileQueue(page, mango);
			await expect(page.getByTestId('queue-item')).toHaveCount(1);
			await expect(nowPlaying).toContainText('Mango Season');
			await expect(page.getByRole('list', { name: 'Playback queue' })).toBeVisible();
			const version = await owner.evaluate((binding) => binding.selectionVersion);
			expect(version).toBe(1);
			if (version === undefined) throw new Error('Missing selection version');
			await returnToMobileLibrary(page);
			libraryMounts += 1;
			const media = await metadataManagementState(page);
			expect(media.calls).toEqual({ play: 0, pause: 0 });
			await page.getByRole('button', { name: 'Apple Cart — Acceptance Band' }).click();
			const appended = { ...mango, ids: ['mango', 'apple'] };
			await expect.poll(() => observedQueue(page)).toEqual(appended);
			expect(await owner.evaluate((binding) => binding.selectionVersion)).toBe(version);
			expect(await owner.evaluate((binding) => binding === window.__favoritesQueueOwner)).toBe(true);
			expect(await metadataManagementState(page)).toEqual(media);
			await expectMobileQueue(page, appended);
			await expect(page.getByTestId('queue-item')).toHaveCount(2);
			expect(await owner.evaluate((binding) => binding === window.__favoritesQueueOwner)).toBe(true);
			// Append has no mounted readout here. Desktop carries that DOM oracle;
			// mobile proves zero owner-selection publications, then real-DOM settling.
			await nowPlaying.evaluate((element) => {
				const observed = element as HTMLElement & { mutations: MutationRecord[]; observer: MutationObserver };
				observed.mutations = [];
				observed.observer = new MutationObserver((records) => observed.mutations.push(...records));
				observed.observer.observe(element, { childList: true, characterData: true, subtree: true });
			});
			await page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
			await expect(page.getByTestId('queue-panel')).toHaveCount(1);
			expect(await nowPlaying.evaluate((element) => {
				const observed = element as HTMLElement & { mutations: MutationRecord[] };
				return observed.mutations.length;
			})).toBe(0);
			await page.screenshot({ path: testInfo.outputPath('queue-appended.png'), fullPage: true });
			await page.getByTestId('player-next').click();
			await expect(nowPlaying).toContainText('Apple Cart');
			await expect(page.getByTestId('queue-panel')).toHaveCount(1);
			expect(await nowPlaying.evaluate((element) => {
				const observed = element as HTMLElement & { mutations: MutationRecord[]; observer: MutationObserver };
				observed.observer.disconnect();
				return observed.mutations.length;
			})).toBeGreaterThan(0);
			expect(await owner.evaluate((binding) => binding.selectionVersion)).toBe(version + 1);
			expect(await owner.evaluate((binding) => binding === window.__favoritesQueueOwner)).toBe(true);
			expect(await observedQueue(page)).toEqual({ ids: ['mango', 'apple'], currentIndex: 1, currentId: 'apple', selectedTitle: 'Apple Cart' });
			expect(await page.evaluate(() => Reflect.get(window, '__managementMedia'))).toEqual(media.calls);
			await returnToMobileLibrary(page);
			libraryMounts += 1;
			await expect(mangoButton).toHaveAccessibleName('Mango Season — Acceptance Band — Queued');
			await expect(mangoButton).toHaveAttribute('aria-pressed', 'true');
			await page.screenshot({ path: testInfo.outputPath('queue-advanced.png'), fullPage: true });
			await testInfo.attach('library-requests', { body: JSON.stringify({ libraryHits, libraryMounts, requests }), contentType: 'application/json' });
			expect(libraryMounts).toBe(4);
			expect(libraryHits).toBe(libraryMounts);
			expect(requests.filter(({ url, type }) => {
				const path = new URL(url).pathname;
				return type === 'media' || /\/(?:media|stream)(?:\/|$)/i.test(path) ||
					(/^\/(?:api|v1)\//.test(path) && path !== '/api/library');
			})).toEqual([]);
		});

		test('404 keeps the existing recovery navigation', async ({ page }, testInfo) => {
			await page.goto('/missing-web-shell-page');
			await expect(page.getByRole('heading', { level: 1 })).toContainText('404');
			await expect(page.getByRole('button', { name: 'Reload' })).toHaveCount(0);
			await page.screenshot({ path: testInfo.outputPath('error-404.png'), fullPage: true });
		});
	});
}
