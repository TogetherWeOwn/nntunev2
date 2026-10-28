import { writeFile } from 'node:fs/promises';
import { expect, test } from '@playwright/test';

test('native pointer-location drop mirrors the picker, then keyboard reselect and Clear stay local', async ({ page, context, baseURL }, testInfo) => {
	expect(['localhost', '127.0.0.1', '[::1]']).toContain(new URL(baseURL!).hostname);
	await page.goto('/library/import');
	await page.waitForLoadState('networkidle');
	const requests: string[] = [];
	page.on('request', (request) => requests.push(`${request.method()} ${request.url()}`));
	const input = page.getByLabel('NZB file', { exact: true });
	const area = page.getByTestId('import-drop-area');
	const box = (await area.boundingBox())!;
	const client = await context.newCDPSession(page);
	const file = testInfo.outputPath('pointer-drop.nzb');
	await writeFile(file, 'synthetic fixture; metadata only');
	await page.evaluate(() => {
		document.addEventListener('drop', (event) => {
			document.documentElement.dataset['trustedFileDrop'] = String(event.isTrusted);
		}, { once: true });
	});
	for (const type of ['dragEnter', 'dragOver', 'drop'] as const) {
		await client.send('Input.dispatchDragEvent', {
			type, x: box.x + box.width / 2, y: box.y + box.height / 2,
			data: { items: [], files: [file], dragOperationsMask: 1 }
		});
	}
	await expect(page.locator('html')).toHaveAttribute('data-trusted-file-drop', 'true');
	await expect(page.getByRole('status')).toContainText('pointer-drop.nzb');
	expect(await input.evaluate((element: HTMLInputElement) => element.files?.[0]?.name))
		.toBe('pointer-drop.nzb');
	await page.getByRole('button', { name: 'Clear selection' }).click();
	await expect(input).toHaveJSProperty('value', '');

	for (let tab = 0; tab < 30 && !await input.evaluate((element) => element === document.activeElement); tab++) {
		await page.keyboard.press('Tab');
	}
	await expect(input).toBeFocused();
	for (let repeat = 0; repeat < 2; repeat++) {
		const chooser = page.waitForEvent('filechooser');
		await page.keyboard.press('Enter');
		await (await chooser).setFiles(file);
		await expect(page.getByRole('status')).toContainText('pointer-drop.nzb');
	}
	await page.keyboard.press('Tab');
	await expect(page.getByRole('button', { name: 'Preview', exact: true })).toBeFocused();
	await page.keyboard.press('Tab');
	await expect(page.getByRole('button', { name: 'Clear selection' })).toBeFocused();
	await page.keyboard.press('Enter');
	await expect(input).toHaveJSProperty('value', '');
	await expect(page.getByTestId('import-status')).toHaveCount(0);
	expect(requests).toEqual([]);
});
