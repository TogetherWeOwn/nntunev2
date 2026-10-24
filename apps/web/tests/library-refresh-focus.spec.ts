import { expect, test, type Page } from '@playwright/test';

/**
 * Library refresh focus: a button-activated refresh restores focus to the
 * Refresh/Retry control only when the loading remount dropped it to the
 * body. Both branches that render that control are pinned — the list
 * branch's Refresh and the error branch's Retry — plus a negative case
 * proving the only-when-lost guard never steals focus the user moved
 * during a slow reload.
 *
 * Complements the TRIM C5 acceptance (button refresh restores focus on the
 * list-branch success path): these cases own the error-branch Retry and
 * the guard's negative half, which that bank does not cover. This file is
 * owned by the queue-trim change, not by the protected TRIM acceptance
 * bank — safe for the Builder to extend.
 *
 * Only the /api/library boundary is mocked; the real page, refresher,
 * Svelte remount and layout controls all run. Every held request is
 * released explicitly; the one bounded wait covers only negative
 * observation (no late focus steal), never a race outcome.
 *
 * Reproduce from repo root (Node 24, pnpm 10; browser installed):
 *   pnpm --dir apps/web install --frozen-lockfile
 *   CI=1 PUBLIC_API_BASE_URL='' pnpm --dir apps/web test:e2e tests/library-refresh-focus.spec.ts --workers=1
 *
 * Breaks caught: error-branch Retry that never restores focus after a
 * failed or successful reload; Refresh-to-401 that strands focus on the
 * body; a deleted only-when-lost guard that rips focus back to Refresh.
 */

const LIBRARY = [
	{ id: 'refresh-focus-alpha', title: 'Alpha', artist: 'Focus Band' },
	{ id: 'refresh-focus-beta', title: 'Beta', artist: 'Focus Band' }
];

function refreshButton(page: Page) {
	return page.getByRole('button', { name: 'Refresh', exact: true });
}

function retryButton(page: Page) {
	return page.getByRole('button', { name: 'Retry', exact: true });
}

function loading(page: Page) {
	return page.getByText('Loading your library…');
}

async function settle(page: Page) {
	await page.evaluate(
		() => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())))
	);
}

test('Retry on the error screen restores focus to itself after a failed reload', async ({
	page
}) => {
	let requests = 0;
	await page.route('**/api/library', (route) => {
		requests += 1;
		return route.fulfill({ status: 500, contentType: 'application/json', body: '{}' });
	});
	await page.goto('/library');
	await expect(page.getByRole('alert')).toContainText(/not reachable/);
	const retry = retryButton(page);
	await expect(retry).toBeVisible();
	await retry.focus();
	await retry.press('Enter');
	await expect.poll(() => requests).toBe(2);
	await expect(loading(page)).toBeHidden();
	await expect(retry).toBeFocused();
});

test('Retry on the error screen moves focus to Refresh after a successful reload', async ({
	page
}) => {
	let requests = 0;
	await page.route('**/api/library', (route) => {
		requests += 1;
		return requests === 1
			? route.fulfill({ status: 500, contentType: 'application/json', body: '{}' })
			: route.fulfill({
					status: 200,
					contentType: 'application/json',
					body: JSON.stringify(LIBRARY)
				});
	});
	await page.goto('/library');
	const retry = retryButton(page);
	await expect(retry).toBeVisible();
	await retry.focus();
	await retry.press('Enter');
	await expect.poll(() => requests).toBe(2);
	await expect(page.getByTestId('library-list')).toBeVisible();
	await expect(refreshButton(page)).toBeFocused();
});

test('Refresh that returns 401 lands focus on the error-branch Retry', async ({ page }) => {
	let requests = 0;
	await page.route('**/api/library', (route) => {
		requests += 1;
		return requests === 1
			? route.fulfill({
					status: 200,
					contentType: 'application/json',
					body: JSON.stringify(LIBRARY)
				})
			: route.fulfill({ status: 401, contentType: 'application/json', body: '{}' });
	});
	await page.goto('/library');
	await expect(page.getByTestId('library-list')).toBeVisible();
	const refresh = refreshButton(page);
	await refresh.focus();
	await refresh.press('Enter');
	await expect.poll(() => requests).toBe(2);
	await expect(page.getByRole('alert')).toContainText(/signed out/);
	await expect(retryButton(page)).toBeFocused();
});

test('Refresh never steals focus the user moved during a slow reload', async ({ page }) => {
	let requests = 0;
	let release!: () => void;
	const gate = new Promise<void>((resolve) => {
		release = resolve;
	});
	await page.route('**/api/library', async (route) => {
		requests += 1;
		if (requests > 1) await gate;
		await route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(LIBRARY)
		});
	});
	try {
		await page.goto('/library');
		await expect(page.getByTestId('library-list')).toBeVisible();
		const refresh = refreshButton(page);
		const appearance = page.locator('#nt-appearance-select');
		await refresh.focus();
		await refresh.press('Enter');
		await expect.poll(() => requests).toBe(2);
		await expect(loading(page)).toBeVisible();
		// The user tabs to a layout control that survives the loading remount.
		await appearance.focus();
		await expect(appearance).toBeFocused();
		release();
		await expect(loading(page)).toBeHidden();
		await settle(page);
		// Bounded negative observation: no late focus steal after completion.
		await page.waitForTimeout(300);
		await expect(appearance).toBeFocused();
		await expect(refresh).not.toBeFocused();
	} finally {
		release();
	}
});
