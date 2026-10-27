import { expect, test as base, type Locator, type Page } from '@playwright/test';

import { dropCases } from './import-drop.cases.js';

/**
 * Protected acceptance handoff: local NZB drag/drop, never upload/import.
 * Builder owns the combined GREEN ready PR; do not open a ready RED-test PR.
 * Protected paths: this spec, import-drop.cases.ts and
 * src/lib/library/import-drop.acceptance.test.ts. Return wrong assertions to
 * their test owner, rather than weakening them in the implementation PR.
 *
 * Run existing package scripts:
 *   pnpm --dir apps/web check
 *   pnpm --dir apps/web test src/lib/library/import-drop.acceptance.test.ts
 *   CI=1 pnpm --dir apps/web test:e2e tests/import-drop.spec.ts --workers=2
 * Pass script arguments directly: an extra standalone -- in this pnpm/toolchain
 * ignores the filters/options and accidentally runs the whole browser suite.
 * Existing import-echo/import-local-selection/import-validation-state specs
 * remain authoritative regressions; do not change their protected assertions.
 *
 * New UI contract: data-testid="import-drop-area" identifies the visible,
 * bounded area containing the retained labeled native input. Styling and copy
 * are the Builder's. Existing import-file/import-status hooks are retained.
 * For assertion-RED on the existing page, dispatch to the real main when the
 * area is absent: validation/outcome assertions then fail on unchanged state,
 * not on a missing selector or a fictional implementation module.
 *
 * DROP-01..06 share independently specified validator boundary fixtures.
 * DROP-00 area; 07 multiple; 08 URL; 09 text; 10 nonfile; 11 directory;
 * 12 clear/reselect; 13 rejected replacement; 14 drag feedback/cancellation;
 * 15 navigation scope; 16 keyboard picker; 17 text escaping; 18 narrow themes.
 * Every browser case also checks zero post-load requests, content reads and
 * persisted library/queue-state changes. Only local build/preview is allowed.
 */

type FileMetadata = { name: string; size: number };
type Payload = { files?: FileMetadata[]; text?: string; url?: string; nonfile?: boolean; directory?: boolean };

const test = base.extend<{ localOnly: void }>({
	localOnly: [async ({ context, page, baseURL }, use, testInfo) => {
		const origin = new URL(baseURL!).origin;
		expect(['localhost', '127.0.0.1', '[::1]']).toContain(new URL(origin).hostname);
		const unsafeRequests: string[] = [];
		const requests: string[] = [];
		context.on('request', (request) => {
			if (!['GET', 'HEAD', 'OPTIONS'].includes(request.method()) || new URL(request.url()).origin !== origin) {
				unsafeRequests.push(`${request.method()} ${request.url()}`);
			}
		});
		await context.route('**/*', (route) => {
			const request = route.request();
			return !['GET', 'HEAD', 'OPTIONS'].includes(request.method()) || new URL(request.url()).origin !== origin
				? route.abort('blockedbyclient') : route.continue();
		});
		await page.goto('/library/import');
		await page.waitForLoadState('networkidle');
		page.on('request', (request) => requests.push(`${request.method()} ${request.url()}`));
		await page.evaluate(() => {
			const reads: string[] = [];
			Object.defineProperty(window, '__dropReads', { value: reads });
			for (const [prototype, methods] of [
				[Blob.prototype, ['text', 'arrayBuffer', 'stream', 'slice', 'bytes']],
				[FileReader.prototype, ['readAsText', 'readAsArrayBuffer', 'readAsDataURL', 'readAsBinaryString']]
			] as const) {
				for (const method of methods) {
					const original = Object.getOwnPropertyDescriptor(prototype, method);
					if (original?.value) Object.defineProperty(prototype, method, {
						...original,
						value: function (this: unknown, ...args: unknown[]) {
							reads.push(method);
							return original.value.apply(this, args);
						}
					});
				}
			}
		});
		const storageBefore = await storageSnapshot(page);
		try {
			await use();
		} finally {
			// Let event-triggered promise/animation work settle before observing.
			await page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
			const reads = await page.evaluate(() => (window as unknown as { __dropReads: string[] }).__dropReads);
			const storageAfter = await storageSnapshot(page);
			await testInfo.attach('drop-local-only-evidence', {
				body: JSON.stringify({ requests, unsafeRequests, reads, storageBefore, storageAfter }, null, 2),
				contentType: 'application/json'
			});
			expect.soft(requests, 'Selection/drop/clear must send zero requests, including GET').toEqual([]);
			expect.soft(unsafeRequests, 'No upload, mutation or external URL fetch, even if blocked').toEqual([]);
			expect.soft(reads, 'Do not read, slice or parse local File contents').toEqual([]);
			expect.soft(storageAfter, 'Do not mutate persisted library/queue state').toEqual(storageBefore);
			expect.soft(new URL(page.url()).pathname, 'Handled drop must not navigate').toBe('/library/import');
		}
	}, { auto: true }]
});

test.use({ serviceWorkers: 'block' });

async function storageSnapshot(page: Page) {
	return page.evaluate(async () => ({
		local: { ...localStorage }, session: { ...sessionStorage },
		databases: (await indexedDB.databases()).map(({ name, version }) => ({ name, version }))
			.sort((a, b) => (a.name ?? '').localeCompare(b.name ?? ''))
	}));
}

async function dropArea(page: Page): Promise<Locator> {
	const area = page.getByTestId('import-drop-area');
	return await area.count() ? area : page.getByRole('main');
}

async function drag(target: Locator, type: string, payload: Payload = {}, relatedToInput = false): Promise<boolean> {
	return target.evaluate((element, { type, payload, relatedToInput }) => {
		const transfer = new DataTransfer();
		for (const file of payload.files ?? []) {
			transfer.items.add(new File([new Uint8Array(file.size)], file.name, { type: 'application/octet-stream' }));
		}
		if (payload.text) transfer.setData('text/plain', payload.text);
		if (payload.url) transfer.setData('text/uri-list', payload.url);
		if (payload.nonfile) transfer.setData('application/x-local-nonfile', 'not a File');
		if (payload.directory) {
			// Browsers cannot construct a directory item. Model only the native
			// boundary: a file-kind directory entry with no actual local File.
			const item = transfer.items.add(new File([], 'directory'));
			Object.defineProperty(item, 'getAsFile', { value: () => null });
			Object.defineProperty(item, 'webkitGetAsEntry', {
				value: () => ({ isDirectory: true, isFile: false, name: 'directory' })
			});
			Object.defineProperty(transfer, 'files', { value: new DataTransfer().files });
		}
		const event = new DragEvent(type, {
			dataTransfer: transfer, bubbles: true, cancelable: true,
			relatedTarget: relatedToInput ? element.querySelector('input') : null
		});
		element.dispatchEvent(event);
		return event.defaultPrevented;
	}, { type, payload, relatedToInput });
}

async function appearance(target: Locator): Promise<string> {
	return target.evaluate((element) => {
		const style = getComputedStyle(element);
		return JSON.stringify([style.backgroundColor, style.borderColor, style.boxShadow, style.outline]);
	});
}

function pickerFile(file: FileMetadata) {
	return { name: file.name, mimeType: 'application/octet-stream', buffer: Buffer.alloc(file.size, 0x78) };
}

async function expectError(page: Page, topic: RegExp) {
	const feedback = page.getByTestId('import-status');
	await expect(feedback).toHaveAttribute('role', 'alert');
	await expect(feedback).toContainText(topic);
	await expect(page.getByRole('status')).toHaveCount(0);
	await expect(feedback).not.toContainText(/\bselected locally\b/i);
}

test('DROP-00 visible bounded drop area retains the labeled single native input and hint', async ({ page }) => {
	const area = page.getByTestId('import-drop-area');
	await expect(area, 'Offer a visible local-file drop target').toBeVisible();
	const input = area.getByLabel('NZB file', { exact: true });
	await expect(input).toHaveAttribute('type', 'file');
	await expect(input).toHaveAttribute('accept', '.nzb');
	await expect(input).not.toHaveAttribute('multiple');
	const bounds = await area.boundingBox();
	expect(bounds!.width).toBeGreaterThan(100);
	expect(bounds!.height).toBeGreaterThan(44);
	const described = await input.getAttribute('aria-describedby');
	expect(described).toBeTruthy();
	const hint = await page.locator(`[id="${described!.split(/\s+/)[0]}"]`).innerText();
	expect(hint).toMatch(/\.nzb/i);
	expect(hint).toMatch(/non-empty|not empty/i);
});

for (const fixture of dropCases) {
	test(`${fixture.id} picker and drop give the same metadata validation feedback: ${fixture.name}`, async ({ page }) => {
		const input = page.getByTestId('import-file');
		const feedback = page.getByTestId('import-status');
		await input.setInputFiles(pickerFile(fixture));
		if (fixture.error) await expectError(page, fixture.error);
		else {
			await expect(feedback).toHaveAttribute('role', 'status');
			await expect(feedback).toContainText(fixture.name);
			await expect(feedback).toContainText(/\d[\d,.]*\s?(?:bytes?|B|KiB|KB|MiB|MB)\b/);
			await expect(feedback).toContainText(/selected locally/i);
			await expect(feedback).toContainText(/(?:not|no|never)[^.!?]*upload/i);
		}
		const pickerFeedback = await feedback.innerText();
		await page.getByRole('button', { name: /^clear/i }).click();
		await expect(feedback).toHaveCount(0);
		const prevented = await drag(await dropArea(page), 'drop', { files: [fixture] });
		// Outcome first: on today's page this fails on real unchanged state.
		await expect(feedback, 'Drop must use the same validation/selection path as the picker').toHaveText(pickerFeedback);
		expect(prevented, 'Handle navigation only inside the drop area').toBe(true);
		await expect(feedback).toHaveAttribute('role', fixture.error ? 'alert' : 'status');
	});
}

const rejectedPayloads: { id: string; name: string; payload: Payload; topic: RegExp }[] = [
	{ id: 'DROP-07', name: 'multiple files', payload: { files: [dropCases[0], dropCases[1]] }, topic: /one|single|multiple/i },
	{ id: 'DROP-08', name: 'a URL', payload: { url: 'https://example.invalid/music.nzb' }, topic: /file|url|link|drop/i },
	{ id: 'DROP-09', name: 'plain text', payload: { text: 'local-album.nzb' }, topic: /file|text|drop/i },
	{ id: 'DROP-10', name: 'nonfile data', payload: { nonfile: true }, topic: /file|drop|unsupported/i },
	{ id: 'DROP-11', name: 'directory-only data', payload: { directory: true }, topic: /file|director|drop/i }
];
for (const fixture of rejectedPayloads) {
	test(`${fixture.id} rejects ${fixture.name} with clear non-success feedback`, async ({ page }) => {
		await page.getByTestId('import-file').setInputFiles(pickerFile(dropCases[0]));
		await drag(await dropArea(page), 'drop', fixture.payload);
		await expectError(page, fixture.topic);
		await expect(page.getByTestId('import-status')).not.toContainText(dropCases[0].name);
	});
}

test('DROP-12 Clear resets picker/drop/highlight/feedback and same-file drop revalidates', async ({ page }) => {
	const area = await dropArea(page);
	const pristine = await appearance(area);
	const input = page.getByTestId('import-file');
	const feedback = page.getByTestId('import-status');
	await input.setInputFiles(pickerFile(dropCases[0]));
	await drag(area, 'dragenter', { files: [dropCases[0]] });
	await page.getByRole('button', { name: /^clear/i }).click();
	await expect(input).toHaveJSProperty('value', '');
	await expect(feedback).toHaveCount(0);
	expect(await appearance(area)).toBe(pristine);
	await drag(area, 'drop', { files: [dropCases[0]] });
	await expect(feedback).toHaveAttribute('role', 'status');
	await page.getByRole('button', { name: /^clear/i }).click();
	await drag(area, 'drop', { files: [dropCases[0]] });
	await expect(feedback).toContainText(dropCases[0].name);
	await drag(area, 'drop', { files: [dropCases[3]] });
	await expectError(page, /empty|no data|0 bytes/i);
	await page.getByRole('button', { name: /^clear/i }).click();
	await drag(area, 'drop', { files: [dropCases[3]] });
	await expectError(page, /empty|no data|0 bytes/i);
});

test('DROP-13 rejected drop replaces previous picker success and keeps feedback association', async ({ page }) => {
	await page.getByTestId('import-file').setInputFiles(pickerFile(dropCases[0]));
	await drag(await dropArea(page), 'drop', { files: [dropCases[5]] });
	await expectError(page, /nzb|extension|type/i);
	const feedback = page.getByTestId('import-status');
	await expect(feedback).not.toContainText(dropCases[0].name);
	const id = await feedback.getAttribute('id');
	expect(id).toBeTruthy();
	const associations = (await page.getByTestId('import-file').getAttribute('aria-describedby') ?? '').split(/\s+/);
	expect(associations).toContain(id);
});

test('DROP-14 drag feedback survives child traversal and resets on leave/drop/dragend/Escape', async ({ page }) => {
	const area = await dropArea(page);
	const pristine = await appearance(area);
	for (const cancellation of ['dragleave', 'drop', 'dragend', 'Escape']) {
		await drag(area, 'dragenter', { files: [dropCases[0]] });
		await expect.poll(() => appearance(area), { message: 'Drag-enter has visible feedback' }).not.toBe(pristine);
		const active = await appearance(area);
		await drag(area, 'dragleave', { files: [dropCases[0]] }, true);
		expect(await appearance(area), 'Moving to the native input does not flicker off').toBe(active);
		if (cancellation === 'Escape') await page.keyboard.press('Escape');
		else await drag(area, cancellation, { files: [dropCases[0]] });
		await expect.poll(() => appearance(area), { message: `${cancellation} clears drag feedback` }).toBe(pristine);
	}
});

test('DROP-15 only handled inside-area drops prevent default navigation', async ({ page }) => {
	const payload = { files: [dropCases[0]] };
	const outsidePrevented = await drag(page.locator('body'), 'drop', payload);
	expect(outsidePrevented, 'Do not install document-wide drop navigation suppression').toBe(false);
	expect(await drag(page.locator('body'), 'dragover', payload), 'Do not suppress unrelated dragover').toBe(false);
	const area = await dropArea(page);
	expect.soft(await drag(area, 'dragover', payload), 'Allow the browser to deliver an inside-area File drop').toBe(true);
	const insidePrevented = await drag(area, 'drop', payload);
	expect(insidePrevented, 'Inside-area drop is handled without navigating').toBe(true);
	await expect(page.getByTestId('import-status')).toHaveAttribute('role', 'status');
});

test('DROP-16 native file picker and Clear remain keyboard operable with visible focus', async ({ page }) => {
	const input = page.getByLabel('NZB file', { exact: true });
	// Reach the native control using real Tab traversal, not programmatic focus.
	for (let tab = 0; tab < 30 && !await input.evaluate((element) => element === document.activeElement); tab++) {
		await page.keyboard.press('Tab');
	}
	await expect(input).toBeFocused();
	expect(await input.evaluate((element) => element.matches(':focus-visible'))).toBe(true);
	const focus = await input.evaluate((element) => {
		const style = getComputedStyle(element);
		return style.outlineStyle !== 'none' && parseFloat(style.outlineWidth) > 0 || style.boxShadow !== 'none';
	});
	expect(focus, 'Keyboard focus must be visibly indicated').toBe(true);
	const chooser = page.waitForEvent('filechooser');
	await page.keyboard.press('Enter');
	await (await chooser).setFiles(pickerFile(dropCases[0]));
	await expect(page.getByTestId('import-status')).toHaveAttribute('role', 'status');
	const clear = page.getByRole('button', { name: /^clear/i });
	await page.keyboard.press('Tab');
	await expect(page.getByRole('button', { name: 'Preview', exact: true })).toBeFocused();
	await page.keyboard.press('Tab');
	await expect(clear).toBeFocused();
	await page.keyboard.press('Enter');
	await expect(input).toHaveJSProperty('value', '');
	await expect(page.getByTestId('import-status')).toHaveCount(0);
});

test('DROP-17 hostile dropped filename is escaped as text, not executable markup', async ({ page }) => {
	const dialogs: string[] = [];
	page.on('dialog', async (dialog) => { dialogs.push(dialog.message()); await dialog.dismiss(); });
	const name = '<svg onload=alert(1)>.nzb';
	await drag(await dropArea(page), 'drop', { files: [{ name, size: 19 }] });
	await expect(page.getByTestId('import-status')).toContainText(name);
	await expect(page.getByTestId('import-status').locator('svg, img, script')).toHaveCount(0);
	expect(dialogs).toEqual([]);
});

test.describe('local NZB selection on touch screens', () => {
	test.use({ hasTouch: true, isMobile: true });
	for (const colorScheme of ['light', 'dark'] as const) {
		test(`DROP-18 ${colorScheme} narrow-screen drop area and native touch fallback fit without overflow`, async ({ page }) => {
			await page.setViewportSize({ width: 320, height: 640 });
			await page.emulateMedia({ colorScheme, reducedMotion: 'reduce' });
			const area = page.getByTestId('import-drop-area');
			await expect(area).toBeVisible();
			await expect(area.getByLabel('NZB file', { exact: true })).toBeVisible();
			const box = await area.boundingBox();
			expect(box!.x).toBeGreaterThanOrEqual(0);
			expect(box!.x + box!.width).toBeLessThanOrEqual(320);
			expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(320);
			await drag(area, 'drop', { files: [{ name: `${'long-name-'.repeat(20)}.nzb`, size: 37 }] });
			await expect(page.getByTestId('import-status')).toHaveAttribute('role', 'status');
			expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(320);
			await page.getByRole('button', { name: /^clear/i }).tap();
			const chooser = page.waitForEvent('filechooser');
			await area.getByLabel('NZB file', { exact: true }).tap();
			await (await chooser).setFiles(pickerFile(dropCases[0]));
			await expect(page.getByTestId('import-status')).toContainText(dropCases[0].name);
		});
	}
});
