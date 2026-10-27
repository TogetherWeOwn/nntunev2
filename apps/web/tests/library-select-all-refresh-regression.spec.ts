import { expect, test } from '@playwright/test';

// Builder-owned regression: selection feedback follows the same refresh
// live-region suspension as the queue, without unmounting the selection bar.
const ITEMS = [
	{ id: 'refresh-alpha', title: 'Alpha Song' },
	{ id: 'refresh-bravo', title: 'Bravo Song' },
	{ id: 'refresh-charlie', title: 'Charlie Song' }
];

for (const outcome of ['success', 'failure'] as const) {
	test(`[SA-R01:${outcome}] selection status suspends during refresh and restores afterward`, async ({ page, context }) => {
		const requests: string[] = [];
		const errors: string[] = [];
		page.on('pageerror', (error) => errors.push(error.message));
		await context.route('**/*', (route) => {
			const url = new URL(route.request().url());
			if (!['localhost', '127.0.0.1'].includes(url.hostname)) return route.abort('blockedbyclient');
			if (url.pathname.startsWith('/api/')) return route.fulfill({ status: 404, json: {} });
			return route.continue();
		});
		let hold: Promise<void> | undefined;
		let release!: () => void;
		let status = 200;
		await page.route('**/api/library', async (route) => {
			requests.push(route.request().method());
			if (hold) await hold;
			await route.fulfill({ status, json: status === 200 ? ITEMS : { error: 'unavailable' } });
		});
		await page.goto('/library');
		await expect(page.getByTestId('library-item')).toHaveCount(3);
		const count = page.getByTestId('selection-count');
		const selectAll = page.locator('main').getByRole('button', { name: 'Select all', exact: true });
		await selectAll.click();
		await expect(count).toHaveText('3 selected');
		await expect(count).toHaveAttribute('role', 'status');

		hold = new Promise<void>((resolve) => { release = resolve; });
		status = outcome === 'success' ? 200 : 503;
		try {
			await page.getByRole('button', { name: 'Refresh', exact: true }).click();
			await expect(page.locator('main').getByRole('status')).toHaveText('Loading your library…');
			await expect(count).toHaveText('3 selected');
			await expect(count).not.toHaveAttribute('role', 'status');
			await expect(count).not.toHaveAttribute('aria-live', 'polite');
			await expect(page.getByTestId('library-item').getByRole('checkbox', { checked: true })).toHaveCount(3);
		} finally {
			release();
		}

		await expect(page.getByText('Loading your library…', { exact: true })).toBeHidden();
		await expect(count).toHaveText('3 selected');
		await expect(count).toHaveAttribute('role', 'status');
		if (outcome === 'failure') await expect(page.getByTestId('library-stale-badge')).toBeVisible();
		await page.getByRole('button', { name: 'Clear selection', exact: true }).click();
		await expect(count).toHaveText('0 selected');
		await expect(count).toHaveAttribute('role', 'status');
		await selectAll.click();
		await expect(count).toHaveText('3 selected');
		await expect(count).toHaveAttribute('role', 'status');
		expect(requests).toEqual(['GET', 'GET']);
		expect(errors).toEqual([]);
	});
}
