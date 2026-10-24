import { expect, test, type Page } from '@playwright/test';

// Builder-owned supplements: storage failures, bounded decoding, trim writers,
// and a late response must not undo the acceptance bank's auth purge contract.
const KEY = 'nntune.library.queue.v1';
const ITEMS = [
	{ id: 'one', title: 'First' },
	{ id: 'two', title: 'Second' },
	{ id: 'three', title: 'Third' }
];

async function library(page: Page): Promise<void> {
	await page.route('**/api/library', (route) =>
		route.fulfill({ json: ITEMS })
	);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(3);
}

async function addAll(page: Page): Promise<void> {
	for (const item of ITEMS) {
		await page.locator(`[data-testid="library-item"][data-id="${item.id}"] > button`).first().click();
	}
}

async function stored(page: Page): Promise<unknown> {
	return page.evaluate((key) => JSON.parse(sessionStorage.getItem(key) ?? 'null'), KEY);
}

test('trim writers persist their retained order and current ID', async ({ page }) => {
	await library(page);
	await addAll(page);
	await page.getByTestId('player-next').click();
	await page.getByRole('button', { name: 'Keep current and upcoming', exact: true }).click();
	expect(await stored(page)).toEqual({ orderIds: ['two', 'three'], currentId: 'two' });
	await page.getByRole('button', { name: 'Clear upcoming', exact: true }).click();
	expect(await stored(page)).toEqual({ orderIds: ['two'], currentId: 'two' });
	await page.reload();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	await expect(page.getByTestId('now-playing')).toContainText('Second');
});

test('an oversized otherwise-valid snapshot is refused before restoring', async ({ page }) => {
	await page.addInitScript(({ key }) => {
		const raw = JSON.stringify({ orderIds: ['two'], currentId: 'two' });
		sessionStorage.setItem(key, raw + ' '.repeat(65_536));
	}, { key: KEY });
	await library(page);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await page.locator('[data-testid="library-item"][data-id="one"] > button').first().click();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	expect(await stored(page)).toEqual({ orderIds: ['one'], currentId: 'one' });
});

test('blocked session-storage getters leave the queue usable', async ({ page }) => {
	await page.addInitScript(() => {
		Object.defineProperty(window, 'sessionStorage', {
			get() { throw new DOMException('Blocked', 'SecurityError'); }
		});
	});
	await library(page);
	await addAll(page);
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(page.getByTestId('now-playing')).toContainText('Second');
});

test('a failed write cannot make Refresh restore an older arrangement', async ({ page }) => {
	await library(page);
	await addAll(page);
	await page.evaluate((key) => {
		const setItem = Storage.prototype.setItem;
		Storage.prototype.setItem = function (name, value) {
			if (this === sessionStorage && name === key) {
				throw new DOMException('Full', 'QuotaExceededError');
			}
			return setItem.call(this, name, value);
		};
	}, KEY);
	await page.getByRole('button', { name: 'Move Third up', exact: true }).click();
	await page.getByTestId('player-next').click();
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(3);
	await expect(page.getByTestId('now-playing')).toContainText('Third');
	expect(await page.getByTestId('queue-item').evaluateAll((rows) =>
		rows.map((row) => row.getAttribute('data-track-id'))
	)).toEqual(['one', 'three', 'two']);
});

test('an auth purge wins over a library response already in flight', async ({ page }) => {
	let release!: () => void;
	const pending = new Promise<void>((resolve) => { release = resolve; });
	await page.addInitScript(({ key }) => {
		sessionStorage.setItem(key, JSON.stringify({ orderIds: ['two'], currentId: 'two' }));
	}, { key: KEY });
	await page.route('**/api/library', async (route) => {
		await pending;
		await route.fulfill({ json: ITEMS }).catch(() => {});
	});
	const requested = page.waitForRequest('**/api/library');
	await page.goto('/library');
	await requested;
	await expect(page.getByText('Loading your library…')).toBeVisible();
	await page.evaluate(() => {
		window.dispatchEvent(new StorageEvent('storage', { key: 'nntune.library.auth-transition.v1' }));
	});
	release();
	await expect(page.getByRole('alert')).toContainText('Your session changed');
	expect(await stored(page)).toBeNull();
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('library-item')).toHaveCount(0);
});
