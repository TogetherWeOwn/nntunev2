import { mkdir, writeFile } from 'node:fs/promises';
import { expect, test as base, type Locator } from '@playwright/test';

// Builder-owned regression coverage supplements the unchanged protected handoff.
const test = base.extend<{ localOnly: void }>({
	localOnly: [async ({ page, context, baseURL }, use, testInfo) => {
		const origin = new URL(baseURL!).origin;
		expect(['localhost', '127.0.0.1', '[::1]']).toContain(new URL(origin).hostname);
		const requests: string[] = [];
		await context.route('**/*', (route) => new URL(route.request().url()).origin === origin
			&& ['GET', 'HEAD', 'OPTIONS'].includes(route.request().method())
			? route.continue() : route.abort('blockedbyclient'));
		await page.goto('/library/import');
		await page.waitForLoadState('networkidle');
		page.on('request', (request) => requests.push(`${request.method()} ${request.url()}`));
		try {
			await use();
		} finally {
			await page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
			await testInfo.attach('local-only-requests', { body: JSON.stringify(requests), contentType: 'application/json' });
			expect(requests, 'Drop and cancellation must send zero requests').toEqual([]);
			expect(new URL(page.url()).pathname).toBe('/library/import');
		}
	}, { auto: true }]
});

test.use({ serviceWorkers: 'block' });

const priorFile = { name: 'prior.nzb', mimeType: 'application/octet-stream', buffer: Buffer.from('synthetic metadata fixture') };

async function pickerState(input: Locator) {
	return input.evaluate((element: HTMLInputElement) => ({
		value: element.value,
		files: Array.from(element.files ?? [], ({ name, size }) => ({ name, size }))
	}));
}

async function appearance(area: Locator) {
	return area.evaluate((element) => {
		const style = getComputedStyle(element);
		return [style.backgroundColor, style.borderColor, style.boxShadow, style.outline];
	});
}

test('trusted directory drop rejects a nonempty otherwise-valid FileList and clears a prior picker', async ({ page, context }, testInfo) => {
	const input = page.getByTestId('import-file');
	await input.setInputFiles(priorFile);
	await expect(page.getByRole('status')).toContainText('prior.nzb');
	const folder = testInfo.outputPath('folder.nzb');
	await mkdir(folder);
	await writeFile(`${folder}/inside.nzb`, 'synthetic directory fixture');
	await page.evaluate(() => {
		document.addEventListener('drop', (event) => {
			const transfer = event.dataTransfer!;
			const item = transfer.items[0];
			const entry = item.webkitGetAsEntry();
			document.documentElement.dataset['directoryDrop'] = JSON.stringify({
				trusted: event.isTrusted,
				files: Array.from(transfer.files, ({ name, size }) => ({ name, size })),
				kind: item.kind, isDirectory: entry?.isDirectory
			});
		}, { once: true, capture: true });
	});
	const area = page.getByTestId('import-drop-area');
	const box = (await area.boundingBox())!;
	const client = await context.newCDPSession(page);
	for (const type of ['dragEnter', 'dragOver', 'drop'] as const) {
		await client.send('Input.dispatchDragEvent', {
			type, x: box.x + box.width / 2, y: box.y + box.height / 2,
			data: { items: [], files: [folder], dragOperationsMask: 1 }
		});
	}
	const receipt = JSON.parse((await page.locator('html').getAttribute('data-directory-drop'))!);
	await testInfo.attach('trusted-directory-metadata', { body: JSON.stringify(receipt), contentType: 'application/json' });
	expect(receipt).toMatchObject({ trusted: true, kind: 'file', isDirectory: true });
	expect(receipt.files).toHaveLength(1);
	expect(receipt.files[0].name).toBe('folder.nzb');
	expect(receipt.files[0].size).toBeGreaterThan(0);
	expect(receipt.files[0].size).toBeLessThanOrEqual(2 * 1024 * 1024);
	await expect(page.getByRole('alert')).toContainText(/file|director|drop/i);
	await expect(page.getByRole('status')).toHaveCount(0);
	await expect(page.getByTestId('import-status')).not.toContainText('prior.nzb');
	expect(await pickerState(input)).toEqual({ value: '', files: [] });
});

for (const payload of ['multiple files', 'URL', 'text', 'nonfile data'] as const) {
	test(`rejected ${payload} drop clears both native picker value and FileList`, async ({ page }) => {
		const input = page.getByTestId('import-file');
		await input.setInputFiles(priorFile);
		await expect(page.getByRole('status')).toContainText('prior.nzb');
		expect((await pickerState(input)).files).toHaveLength(1);
		await page.getByTestId('import-drop-area').evaluate((element, payload) => {
			const transfer = new DataTransfer();
			if (payload === 'multiple files') {
				for (const name of ['first.nzb', 'second.nzb']) transfer.items.add(new File(['fixture'], name));
			} else if (payload === 'URL') transfer.setData('text/uri-list', 'https://example.invalid/music.nzb');
			else if (payload === 'text') transfer.setData('text/plain', 'music.nzb');
			else transfer.setData('application/x-local-nonfile', 'not a File');
			element.dispatchEvent(new DragEvent('drop', { dataTransfer: transfer, bubbles: true, cancelable: true }));
		}, payload);
		await expect(page.getByRole('alert')).toContainText(/one|single|file|drop/i);
		await expect(page.getByRole('status')).toHaveCount(0);
		await expect(page.getByTestId('import-status')).not.toContainText('prior.nzb');
		expect(await pickerState(input)).toEqual({ value: '', files: [] });
	});
}

for (const reset of ['chooser cancel', 'window blur'] as const) {
	for (const selection of ['none', 'valid', 'invalid'] as const) {
		test(`${reset} restores idle drag appearance and preserves ${selection} selection feedback`, async ({ page }) => {
			const input = page.getByTestId('import-file');
			const feedback = page.getByTestId('import-status');
			if (selection !== 'none') {
				await input.setInputFiles({ ...priorFile, name: selection === 'valid' ? 'prior.nzb' : 'prior.txt' });
				await expect(feedback).toHaveAttribute('role', selection === 'valid' ? 'status' : 'alert');
			} else await expect(feedback).toHaveCount(0);
			const pickerBefore = await pickerState(input);
			const feedbackBefore = await feedback.allTextContents();
			const associationsBefore = await input.getAttribute('aria-describedby');
			const area = page.getByTestId('import-drop-area');
			const idle = await appearance(area);
			await area.dispatchEvent('dragenter', { dataTransfer: await page.evaluateHandle(() => new DataTransfer()) });
			await expect.poll(() => appearance(area), { message: 'Start with visible active drag feedback' }).not.toEqual(idle);
			// Event-level reset coverage, not automation of an OS chooser dialog.
			if (reset === 'chooser cancel') await input.dispatchEvent('cancel');
			else await page.evaluate(() => window.dispatchEvent(new Event('blur')));
			await expect.poll(() => appearance(area), { message: `${reset} restores idle appearance` }).toEqual(idle);
			expect(await pickerState(input)).toEqual(pickerBefore);
			expect(await feedback.allTextContents()).toEqual(feedbackBefore);
			if (selection !== 'none') await expect(feedback).toHaveAttribute('role', selection === 'valid' ? 'status' : 'alert');
			else await expect(feedback).toHaveCount(0);
			await expect(input).toHaveAttribute('aria-describedby', associationsBefore!);
		});
	}
}
