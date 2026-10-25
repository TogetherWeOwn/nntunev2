import { expect, test, type Page } from '@playwright/test';

const TEXT = '1. Demo track — Fixture Band\n2. Second Signal — Fixture Band\n3. Third Light — Session Players';

async function openQueue(page: Page, delayed = false) {
	await page.route('**/api/demo/audio**', () => {});
	await page.addInitScript((defer) => {
		const pending: { resolve: () => void; reject: () => void }[] = [];
		const calls: string[] = [];
		Object.assign(window, { queueExportProbe: { pending, calls } });
		Object.defineProperty(navigator, 'clipboard', {
			configurable: true,
			value: { writeText: (text: string) => {
				calls.push(text);
				return defer ? new Promise<void>((resolve, reject) => {
					pending.push({ resolve, reject: () => reject(new Error('Denied')) });
				}) : Promise.resolve();
			} }
		});
		HTMLMediaElement.prototype.play = () => new Promise<void>(() => {});
	}, delayed);
	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
}

async function settle(page: Page, index: number, reject = false) {
	await page.evaluate(({ index, reject }) => {
		const probe = (window as unknown as { queueExportProbe: {
			pending: { resolve: () => void; reject: () => void }[];
		} }).queueExportProbe;
		const write = probe.pending[index];
		if (!write) throw new Error('Expected pending write');
		if (reject) write.reject(); else write.resolve();
	}, { index, reject });
}

test('filtered queue still exports every track, and a late rejection cannot restore stale fallback', async ({ page }) => {
	await openQueue(page, true);
	await page.getByLabel('Filter queue', { exact: true }).fill('Third Light');
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	await page.getByTestId('queue-export-copy').click();
	const calls = await page.evaluate(() => (window as unknown as { queueExportProbe: { calls: string[] } }).queueExportProbe.calls);
	expect(calls).toEqual([TEXT]);
	await page.getByTestId('queue-move-up-third-light').click();
	await settle(page, 0, true);
	await expect(page.getByTestId('queue-export-status')).toHaveText('');
	await expect(page.getByTestId('queue-export-fallback')).toHaveCount(0);
});

test('a newer copy owns feedback even when an older write rejects afterwards', async ({ page }) => {
	await openQueue(page, true);
	await page.getByTestId('queue-export-copy').click();
	await page.getByTestId('queue-export-copy').click();
	await settle(page, 1);
	await expect(page.getByTestId('queue-export-status')).toHaveText('Copied queue as text.');
	await settle(page, 0, true);
	await expect(page.getByTestId('queue-export-status')).toHaveText('Copied queue as text.');
	await expect(page.getByTestId('queue-export-fallback')).toHaveCount(0);
});

test('download supersedes a pending copy, revokes its Blob URL, and removes its temporary link', async ({ page }) => {
	await openQueue(page, true);
	await page.evaluate(() => {
		const probe = { created: [] as string[], revoked: [] as string[] };
		Object.assign(window, { queueDownloadProbe: probe });
		const create = URL.createObjectURL.bind(URL);
		const revoke = URL.revokeObjectURL.bind(URL);
		URL.createObjectURL = (blob) => {
			const url = create(blob);
			probe.created.push(url);
			return url;
		};
		URL.revokeObjectURL = (url) => { probe.revoked.push(url); revoke(url); };
	});
	await page.getByTestId('queue-export-copy').click();
	const downloaded = page.waitForEvent('download');
	await page.getByTestId('queue-export-download').click();
	expect((await downloaded).suggestedFilename()).toBe('queue.m3u');
	await settle(page, 0, true);
	await expect(page.getByTestId('queue-export-status')).toContainText('Download started.');
	await expect(page.getByTestId('queue-export-fallback')).toHaveCount(0);
	await expect(page.locator('a[download="queue.m3u"]')).toHaveCount(0);
	const secondDownload = page.waitForEvent('download');
	await page.getByTestId('queue-export-download').click();
	await secondDownload;
	const urls = await page.evaluate(() => (window as unknown as {
		queueDownloadProbe: { created: string[]; revoked: string[] };
	}).queueDownloadProbe);
	expect(urls.created).toHaveLength(2);
	expect(urls.revoked).toEqual([urls.created[0]]);
	// A same-document SvelteKit navigation destroys the player component;
	// the window probe survives so cleanup of the final URL is observable.
	await page.evaluate(() => {
		const link = document.createElement('a');
		link.href = '/';
		document.body.append(link);
		link.click();
		link.remove();
	});
	await page.waitForURL('**/');
	await expect.poll(() => page.evaluate(() => (window as unknown as {
		queueDownloadProbe: { revoked: string[] };
	}).queueDownloadProbe.revoked)).toEqual(urls.created);
});

test('Blob creation failure reports refusal, never a saved-file claim, and the next copy works', async ({ page }) => {
	await openQueue(page);
	await page.evaluate(() => { URL.createObjectURL = () => { throw new Error('Unavailable'); }; });
	await page.getByTestId('queue-export-download').click();
	await expect(page.getByTestId('queue-export-status')).toContainText('Could not start the download.');
	await expect(page.getByTestId('queue-export-status')).not.toContainText(/saved|downloaded|copied/i);
	await expect(page.locator('a[download="queue.m3u"]')).toHaveCount(0);
	await page.getByTestId('queue-export-copy').click();
	await expect(page.getByTestId('queue-export-status')).toHaveText('Copied queue as text.');
});

test('320px actions wrap, and denied clipboard text stays keyboard-selectable without transport shortcuts', async ({ page }) => {
	await page.setViewportSize({ width: 320, height: 800 });
	await openQueue(page, true);
	for (const id of ['queue-export-copy', 'queue-export-download']) {
		const box = await page.getByTestId(id).boundingBox();
		expect(box).not.toBeNull();
		expect(box!.x).toBeGreaterThanOrEqual(0);
		expect(box!.x + box!.width).toBeLessThanOrEqual(320);
		expect(box!.height).toBeGreaterThanOrEqual(44);
	}
	await page.getByTestId('queue-export-copy').click();
	await settle(page, 0, true);
	await expect(page.getByTestId('queue-export-copy')).toBeEnabled();
	const fallback = page.getByTestId('queue-export-fallback');
	await fallback.focus();
	await page.keyboard.press('Control+a');
	await expect(fallback).toHaveValue(TEXT);
	const selected = await fallback.evaluate((el: HTMLTextAreaElement) => ({ start: el.selectionStart, end: el.selectionEnd }));
	expect(selected).toEqual({ start: 0, end: TEXT.length });
	const status = await page.getByTestId('player-status').textContent();
	await page.keyboard.press('Space');
	await expect(page.getByTestId('player-status')).toHaveText(status ?? '');
});
