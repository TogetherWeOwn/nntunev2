import { expect, test, type Page } from '@playwright/test';

// Supplementary lifecycle regressions; the protected acceptance bank stays unchanged.
const items = [{ id: 'export-one', title: 'One', artist: 'Artist' }];
const text = '1. One — Artist';

async function open(page: Page) {
	let requests = 0;
	await page.route('**/api/library', (route) => {
		requests += 1;
		return route.fulfill({ json: items });
	});
	await page.goto('/library');
	await page.getByRole('checkbox', { name: 'Select One', exact: true }).check();
	return () => requests;
}

async function refresh(page: Page, requests: () => number) {
	const before = requests();
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect.poll(requests).toBe(before + 1);
	await expect(page.getByRole('button', { name: 'Refresh', exact: true })).toBeEnabled();
	await expect(page.getByText('Loading your library…', { exact: true })).toHaveCount(0);
}

test('content-identical refresh preserves selectable clipboard fallback', async ({ page }) => {
	await page.addInitScript(() => {
		Object.defineProperty(navigator, 'clipboard', { value: undefined, configurable: true });
	});
	const requests = await open(page);
	await page.getByTestId('selection-export-copy').click();
	await expect(page.getByTestId('selection-export-fallback')).toHaveValue(text);
	await refresh(page, requests);
	await expect(page.getByTestId('selection-export-fallback')).toHaveValue(text);
});

test('content-identical refresh preserves pending copy but changed selection invalidates it', async ({ page }) => {
	await page.addInitScript(() => {
		Object.defineProperty(navigator, 'clipboard', {
			configurable: true,
			value: {
				writeText: () => new Promise<void>((resolve) => {
					(window as unknown as { settleCopy: () => void }).settleCopy = resolve;
				})
			}
		});
	});
	const requests = await open(page);
	await page.getByTestId('selection-export-copy').click();
	await refresh(page, requests);
	await page.evaluate(() => (window as unknown as { settleCopy: () => void }).settleCopy());
	await expect(page.getByTestId('selection-export-status')).toHaveText('Copied selected tracks as text.');

	await page.getByTestId('selection-export-copy').click();
	await page.getByRole('button', { name: 'Clear selection', exact: true }).click();
	await page.evaluate(() => (window as unknown as { settleCopy: () => void }).settleCopy());
	await expect(page.getByTestId('selection-export-status')).toHaveText('');
});

test('changing the selection clears stale fallback text and status', async ({ page }) => {
	await page.addInitScript(() => {
		let reject = true;
		Object.defineProperty(navigator, 'clipboard', {
			configurable: true,
			value: {
				writeText: () => (reject ? Promise.reject(new Error('denied')) : Promise.resolve())
			}
		});
		(window as unknown as { allowCopy: () => void }).allowCopy = () => {
			reject = false;
		};
	});
	await page.route('**/api/library', (route) =>
		route.fulfill({ json: [...items, { id: 'export-two', title: 'Two', artist: 'Artist' }] })
	);
	await page.goto('/library');
	await page.getByRole('checkbox', { name: 'Select One', exact: true }).check();

	await page.getByTestId('selection-export-copy').click();
	await expect(page.getByTestId('selection-export-fallback')).toHaveValue(text);
	await page.getByRole('checkbox', { name: 'Select Two', exact: true }).check();
	await expect(page.getByTestId('selection-export-fallback')).toHaveCount(0);
	await expect(page.getByTestId('selection-export-status')).toHaveText('');

	await page.evaluate(() => (window as unknown as { allowCopy: () => void }).allowCopy());
	await page.getByTestId('selection-export-copy').click();
	await expect(page.getByTestId('selection-export-status')).toHaveText('Copied selected tracks as text.');
	await page.getByRole('checkbox', { name: 'Select Two', exact: true }).uncheck();
	await expect(page.getByTestId('selection-export-status')).toHaveText('');
});

test('auth teardown revokes the retained export Blob URL', async ({ page }) => {
	await page.addInitScript(() => {
		const live: string[] = [];
		(window as unknown as { exportBlobs: string[] }).exportBlobs = live;
		const create = URL.createObjectURL.bind(URL);
		const revoke = URL.revokeObjectURL.bind(URL);
		URL.createObjectURL = (blob) => {
			const url = create(blob);
			live.push(url);
			return url;
		};
		URL.revokeObjectURL = (url) => {
			const index = live.indexOf(url);
			if (index !== -1) live.splice(index, 1);
			revoke(url);
		};
	});
	await open(page);
	const download = page.waitForEvent('download');
	await page.getByTestId('selection-export-download').click();
	await download;
	const liveCount = () => page.evaluate(() => (window as unknown as { exportBlobs: string[] }).exportBlobs.length);
	await expect.poll(liveCount).toBe(1);
	await page.evaluate(() => window.dispatchEvent(new StorageEvent('storage', { key: 'nntune.library.auth-transition.v1', newValue: 'next-account' })));
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect.poll(liveCount).toBe(0);
});
