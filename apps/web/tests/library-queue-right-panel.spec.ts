import { expect, test, type Page } from '@playwright/test';

const tracks = [
	{ id: 'coral', title: 'Coral', streamUrl: '/api/demo/audio?track=coral' },
	{ id: 'moss', title: 'Moss' },
	{ id: 'fern', title: 'Fern' }
];
const toggle = (page: Page) => page.getByRole('button', { name: 'Queue in side panel' });
const panel = (page: Page) => page.getByTestId('right-panel');
const close = (page: Page) => panel(page).getByRole('button', { name: 'Close queue panel' });
const next = (page: Page) => page.getByTestId('player-next');
const inline = (page: Page) => page.getByRole('main').getByTestId('player-next');
const queued = (page: Page) => page.getByTestId('queue-item');

async function seed(page: Page, pauseRefresh?: Promise<void>): Promise<void> {
	let requests = 0;
	await page.route('**/api/library', async (route) => {
		if (requests++ > 0) await pauseRefresh;
		await route.fulfill({ json: tracks });
	});
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(tracks.length);
	for (const { id } of tracks) {
		await page.locator(`[data-testid="library-item"][data-id="${id}"]`).click();
	}
	await expect(queued(page)).toHaveCount(tracks.length);
}

async function keyboardOpen(page: Page): Promise<void> {
	await toggle(page).focus();
	await page.keyboard.press('Enter');
	await expect(close(page)).toBeFocused();
	await expect(toggle(page)).toHaveAttribute('aria-expanded', 'true');
}

function order(page: Page): Promise<(string | null)[]> {
	return queued(page).evaluateAll((rows) => rows.map((row) => row.getAttribute('data-track-id')));
}

test('keyboard moves the same live queue to the right slot and back', async ({ page }) => {
	await page.setViewportSize({ width: 1280, height: 800 });
	await seed(page);
	await expect(panel(page)).toHaveCount(0);
	await expect(inline(page)).toBeVisible();
	await page.getByRole('checkbox', { name: 'Select Fern for queue actions' }).check();
	const originalNext = (await next(page).elementHandle())!;
	const audio = page.getByTestId('audio-player');
	await expect(audio).toHaveCount(1);
	const originalAudio = (await audio.elementHandle())!;

	await keyboardOpen(page);
	await expect(inline(page)).toHaveCount(0);
	await expect(next(page)).toHaveCount(1);
	await expect(page.getByTestId('now-playing')).toHaveCount(1);
	expect(await originalNext.evaluate((node) => !!node.closest('[data-testid="right-panel"]'))).toBe(true);
	expect(await originalAudio.evaluate((node) => node.isConnected)).toBe(true);
	await expect(audio).toHaveCount(1);
	await expect(page.getByRole('checkbox', { name: 'Select Fern for queue actions' })).toBeChecked();
	expect(await order(page)).toEqual(['coral', 'moss', 'fern']);

	const sidebar = (await panel(page).boundingBox())!;
	const main = (await page.getByRole('main').boundingBox())!;
	expect(sidebar.x).toBeGreaterThanOrEqual(main.x + main.width - 1);
	await page.keyboard.press('Tab');
	await expect(next(page)).toBeFocused();
	await page.keyboard.press('Enter');
	await expect(page.getByTestId('now-playing')).toContainText('Moss');
	await expect(panel(page).locator('[data-testid="queue-item"][aria-current="true"]')).toHaveAttribute('data-track-id', 'moss');
	expect(await order(page)).toEqual(['coral', 'moss', 'fern']);

	await page.keyboard.press('Escape');
	await expect(toggle(page)).toBeFocused();
	await expect(toggle(page)).toHaveAttribute('aria-expanded', 'false');
	await expect(panel(page)).toHaveCount(0);
	await expect(inline(page)).toBeVisible();
	expect(await originalNext.evaluate((node) => !!node.closest('main'))).toBe(true);
	await expect(page.getByRole('checkbox', { name: 'Select Fern for queue actions' })).toBeChecked();

	await keyboardOpen(page);
	await page.keyboard.press('Space');
	await expect(toggle(page)).toBeFocused();
	await expect(panel(page)).toHaveCount(0);
});

test('the panel keeps order actions, export fallback and loading behavior live', async ({ page }) => {
	let finishRefresh!: () => void;
	const pauseRefresh = new Promise<void>((resolve) => (finishRefresh = resolve));
	await page.addInitScript(() => {
		Object.defineProperty(navigator, 'clipboard', { value: undefined });
	});
	await seed(page, pauseRefresh);
	await keyboardOpen(page);
	await panel(page).getByRole('button', { name: 'More actions for Fern' }).click();
	await panel(page).getByRole('menuitem', { name: 'Move to top' }).click();
	expect(await order(page)).toEqual(['fern', 'coral', 'moss']);
	await panel(page).getByRole('button', { name: 'More actions for Fern' }).click();
	await panel(page).getByRole('menuitem', { name: 'Copy link' }).click();
	await expect(page.getByTestId('queue-share-fallback')).toHaveValue(/\/library\?track=fern/);
	await panel(page).getByRole('button', { name: 'Copy queue as text' }).click();
	await expect(panel(page).getByTestId('queue-export-fallback')).toHaveValue(/Fern[\s\S]*Coral[\s\S]*Moss/);
	const download = page.waitForEvent('download');
	await panel(page).getByRole('button', { name: 'Download queue (.m3u)' }).click();
	expect((await download).suggestedFilename()).toBe('queue.m3u');

	try {
		await page.getByRole('button', { name: 'Refresh', exact: true }).click();
		await expect(panel(page).getByTestId('now-playing')).not.toHaveAttribute('role', 'status');
		await expect(panel(page).getByTestId('queue-item')).toHaveCount(3);
	} finally {
		finishRefresh();
	}
	await expect(panel(page).getByTestId('now-playing')).toHaveAttribute('role', 'status');
});

test('small screens keep one inline queue and resizing recovers focus', async ({ page }) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await seed(page);
	await expect(toggle(page)).toBeHidden();
	await expect(panel(page)).toHaveCount(0);
	await expect(inline(page)).toBeVisible();
	await inline(page).click();
	await expect(page.getByTestId('now-playing')).toContainText('Moss');

	await page.setViewportSize({ width: 1280, height: 800 });
	await keyboardOpen(page);
	await next(page).focus();
	await page.setViewportSize({ width: 799, height: 844 });
	await expect(toggle(page)).toBeHidden();
	await expect(panel(page)).toHaveCount(0);
	await expect(inline(page)).toBeFocused();
	await expect(next(page)).toHaveCount(1);
	await expect(page.getByTestId('now-playing')).toContainText('Moss');
});

test('an auth boundary or leaving Library releases the panel slot', async ({ page }) => {
	await seed(page);
	await keyboardOpen(page);
	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate(() => window.localStorage.setItem('nntune.library.auth-transition.v1', 'fixture'));
	await otherTab.close();
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(panel(page)).toHaveCount(0);
	await expect(queued(page)).toHaveCount(0);

	await page.getByRole('button', { name: 'Retry' }).click();
	await keyboardOpen(page);
	await page.getByTestId('shell-rail').getByRole('link', { name: 'Home' }).click();
	await expect(page).toHaveURL(/\/home$/);
	await expect(panel(page)).toHaveCount(0);
	await expect(next(page)).toHaveCount(0);
});
