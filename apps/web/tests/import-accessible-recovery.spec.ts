import { expect, test as base, type Locator, type Page } from '@playwright/test';

/**
 * Protected acceptance handoff: accessible recovery on the local NZB import
 * page — validation state, repeated feedback and Clear focus.
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build lands on top of
 * this branch and must make these tests pass WITHOUT editing this file; a
 * wrong assertion comes back to the acceptance card instead. Keep the prior
 * import banks (import-echo, import-drop*, import-local-selection,
 * import-validation-state) unmodified: this bank layers only the new
 * recovery behavior on their existing contracts.
 *
 * Stable case IDs (IMPORTRECOVERY-01..09):
 * - 01 pristine input is not marked invalid; a rejected picker selection
 *   sets aria-invalid="true" with the error in aria-describedby alongside
 *   the requirements hint.
 * - 02 a valid selection clears aria-invalid; Clear also restores the
 *   pristine invalid-state semantics.
 * - 03 picker and drop share the same aria-invalid outcome (invalid and
 *   valid paths agree through both entry points).
 * - 04 an identical repeated explicit selection/drop refreshes the same
 *   stable feedback region (one region, not duplicated; an empty-commit
 *   refresh is fine, exactly one non-empty/redundant outcome per action).
 * - 05 Clear announces one short cleared outcome through a distinct
 *   outcome region while the file-specific import-status feedback
 *   disappears; idle rendering does not re-announce either region.
 * - 06 keyboard-activating the focused Clear button returns focus to the
 *   native file input after Clear unmounts; picker/drop/hover and the
 *   chooser never steal focus; the same file can be re-selected after.
 * - 07 drag hover changes nothing about focus, validation state or the
 *   current feedback.
 * - 08 hostile filenames stay escaped text (valid and rejected shapes) and
 *   a truncated import-status toggle cannot inject markup or dialogs.
 * - 09 320px light/dark runtime: aria-invalid, one clear outcome and
 *   post-clear input focus hold without horizontal overflow.
 *
 * Contract notes for the builder (exact wording and DOM shape are yours):
 * - aria-invalid lives on the native input (data-testid="import-file"):
 *   absent or "false" while pristine/valid, "true" only after the current
 *   selection actually fails validation.
 * - The file-specific feedback keeps data-testid="import-status" with
 *   role=status (valid) / role=alert (invalid), led in aria-describedby by
 *   the static requirements hint, exactly as the prior banks pin.
 * - The cleared outcome is a DISTINCT live region (not import-status, which
 *   must keep disappearing on clear): role=status or aria-live=polite,
 *   never assertive, concise text that mentions clearing the local
 *   selection. One region handles repeated clears; silent mutation churn
 *   outside the commit is not rejected, but no second/redundant outcome
 *   may be announced for one activation, and idle frames must not mutate.
 * - Repeated outcomes refresh the SAME feedback element: identical
 *   consecutive text must still produce a fresh commit for assistive tech
 *   (clear-then-set or replace). One polite/status commit per action is
 *   enough; the file-specific alert keeps the prior banks' role=alert.
 * - After Clear activates, focus ends on the native file input once
 *   rendering settles. Do not move focus for picker selections, dropped
 *   files, chooser cancellation, or drag hover.
 * - Do not read file contents, parse, upload, fetch, or mutate persisted
 *   state. Every case asserts zero post-load requests and zero content
 *   reads through the shared fixture.
 *
 * Baseline RED on the current page is assertion-level only: 01/02/03 fail
 * on the missing aria-invalid, 04 on the missing fresh commit, 05 on the
 * missing distinct clear outcome, 06 on focus lost to body. Cases 07/08
 * are positive controls that already pass; 09 combines the new contract.
 *
 * Focused run (syntax governed by the repo Playwright config):
 *   CI=1 PUBLIC_API_BASE_URL='' pnpm --dir apps/web exec playwright test tests/import-accessible-recovery.spec.ts --workers=1 --retries=0
 */

type FileMetadata = { name: string; size: number };
type ProbeWindow = Window & {
	__recoveryReads: string[];
	__recoverySnapshots: Map<Element, string[]>;
	__recoveryIdleMutations: number;
};

const VALID: FileMetadata = { name: 'recovery-valid.nzb', size: 4096 };
const EMPTY: FileMetadata = { name: 'recovery-empty.nzb', size: 0 };
const WRONG_EXT: FileMetadata = { name: 'recovery-notes.txt', size: 64 };

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
			Object.defineProperty(window, '__recoveryReads', { value: reads });
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
			await page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
			const reads = await page.evaluate(() => (window as unknown as { __recoveryReads: string[] }).__recoveryReads);
			const storageAfter = await storageSnapshot(page);
			await testInfo.attach('import-recovery-local-only-evidence', {
				body: JSON.stringify({ requests, unsafeRequests, reads, storageBefore, storageAfter }, null, 2),
				contentType: 'application/json'
			});
			expect.soft(requests, 'Selection/drop/clear must send zero requests, including GET').toEqual([]);
			expect.soft(unsafeRequests, 'No upload, mutation or external URL fetch, even if blocked').toEqual([]);
			expect.soft(reads, 'Do not read, slice or parse local File contents').toEqual([]);
			expect.soft(storageAfter, 'Do not mutate persisted library/queue state').toEqual(storageBefore);
			expect.soft(new URL(page.url()).pathname, 'Handled actions must not navigate').toBe('/library/import');
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

function pickerFile(file: FileMetadata) {
	return { name: file.name, mimeType: 'application/octet-stream', buffer: Buffer.alloc(file.size, 0x78) };
}

async function dropFiles(page: Page, files: FileMetadata[]): Promise<void> {
	const area = page.getByTestId('import-drop-area');
	const target = (await area.count()) ? area : page.getByRole('main');
	await target.evaluate((element, payload) => {
		const transfer = new DataTransfer();
		for (const file of payload) {
			transfer.items.add(new File([new Uint8Array(file.size)], file.name, { type: 'application/octet-stream' }));
		}
		element.dispatchEvent(new DragEvent('drop', { dataTransfer: transfer, bubbles: true, cancelable: true }));
	}, files);
}

async function dragHover(page: Page): Promise<void> {
	const area = page.getByTestId('import-drop-area');
	const target = (await area.count()) ? area : page.getByRole('main');
	for (const type of ['dragenter', 'dragover', 'dragleave'] as const) {
		await target.evaluate((element, eventType) => {
			const transfer = new DataTransfer();
			transfer.items.add(new File([new Uint8Array(4)], 'hover.nzb', { type: 'application/octet-stream' }));
			element.dispatchEvent(new DragEvent(eventType, { dataTransfer: transfer, bubbles: true, cancelable: true }));
		}, type);
	}
}

async function describedByIds(input: Locator): Promise<string[]> {
	return ((await input.getAttribute('aria-describedby')) ?? '').split(/\s+/).filter(Boolean);
}

async function expectNotInvalid(input: Locator, message: string): Promise<void> {
	const value = (await input.getAttribute('aria-invalid'))?.toLowerCase() ?? null;
	expect(value, message).not.toBe('true');
}

async function expectInvalid(input: Locator, message: string): Promise<void> {
	await expect(input, message).toHaveAttribute('aria-invalid', 'true');
}

/** Track per-element settled text commits on a live region. */
async function observeCommits(region: Locator): Promise<void> {
	await region.evaluate((element) => {
		const win = window as unknown as ProbeWindow;
		win.__recoverySnapshots ??= new Map<Element, string[]>();
		const commits: string[] = [];
		win.__recoverySnapshots.set(element, commits);
		const observer = new MutationObserver(() => commits.push(element.textContent ?? ''));
		observer.observe(element, { childList: true, characterData: true, subtree: true });
	});
}

async function commitsOf(region: Locator): Promise<{ all: string[]; nonEmpty: string[] }> {
	return region.evaluate((element) => {
		const win = window as unknown as ProbeWindow;
		const all = [...(win.__recoverySnapshots?.get(element) ?? [])];
		return { all, nonEmpty: all.filter((entry) => entry.trim().length > 0) };
	});
}

/** Count mutations inside main live regions while the page sits idle. */
async function observeIdleMutations(page: Page): Promise<void> {
	await page.evaluate(() => {
		const win = window as unknown as ProbeWindow;
		win.__recoveryIdleMutations = 0;
		const selector = 'main [role="status"], main [role="alert"], main [aria-live="polite"], main [aria-live="assertive"]';
		new MutationObserver((records) => {
			for (const record of records) {
				const target = record.target instanceof Element ? record.target : record.target.parentElement;
				if (target?.closest(selector)) win.__recoveryIdleMutations += 1;
			}
		}).observe(document.querySelector('main')!, { subtree: true, childList: true, characterData: true, attributes: true });
	});
}

async function idleMutations(page: Page): Promise<number> {
	return page.evaluate(async () => {
		await new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
		return (window as unknown as ProbeWindow).__recoveryIdleMutations ?? 0;
	});
}

/** The distinct clear-outcome region: a polite/status region inside main
 *  that is not the file-specific import-status feedback. */
async function clearOutcome(page: Page): Promise<{ locator: Locator; text: string }> {
	const candidates = page.locator(
		'main [role="status"]:not([data-testid="import-status"]), main [aria-live="polite"]:not([data-testid="import-status"])'
	);
	const total = await candidates.count();
	expect(total, 'Exactly one distinct short-outcome region handles the cleared selection').toBe(1);
	const locator = candidates.first();
	await expect(locator, 'The distinct outcome region is never assertive').not.toHaveAttribute('aria-live', 'assertive');
	await expect(locator, 'The distinct outcome region is not an alert').not.toHaveAttribute('role', 'alert');
	const text = ((await locator.textContent()) ?? '').trim();
	expect(text.length, 'The cleared outcome stays short').toBeLessThanOrEqual(120);
	expect(text, 'The cleared outcome says the local selection was cleared').toMatch(
		/(?:selection|file|import)[^.!?]*\b(?:clear(?:ed|s)?|removed)\b|\b(?:clear(?:ed|s)?|removed)\b[^.!?]*(?:selection|file|import)\b/i
	);
	expect(text, 'The cleared outcome keeps the local-only framing').toMatch(/\blocal(?:ly)?\b/i);
	return { locator, text };
}

test('IMPORTRECOVERY-01 pristine input is not invalid; a rejected selection sets aria-invalid with the error described', async ({ page }) => {
	const input = page.getByTestId('import-file');
	const feedback = page.getByTestId('import-status');
	await expectNotInvalid(input, 'Initial untouched state must not announce invalid');
	await expect(feedback).toHaveCount(0);
	const pristineDescribed = await describedByIds(input);

	await input.setInputFiles(pickerFile(WRONG_EXT));
	await expect(feedback).toHaveAttribute('role', 'alert');
	await expect(feedback).toContainText(/nzb|extension|type/i);
	await expectInvalid(input, 'Failed validation marks the input invalid');
	const errorDescribed = await describedByIds(input);
	const feedbackId = await feedback.getAttribute('id');
	expect(feedbackId, 'Feedback keeps a stable id for the description association').toBeTruthy();
	expect(errorDescribed, 'Requirements hint stays first in the description').toEqual([
		...pristineDescribed, feedbackId as string
	]);
	const hint = await page.locator(`[id="${errorDescribed[0]}"]`).innerText();
	expect(hint).toMatch(/\.nzb/i);
	expect(hint).toMatch(/non-empty|not empty/i);
});

test('IMPORTRECOVERY-02 a valid selection and Clear both remove the invalid state', async ({ page }) => {
	const input = page.getByTestId('import-file');
	const feedback = page.getByTestId('import-status');
	const pristineDescribed = await describedByIds(input);

	await input.setInputFiles(pickerFile(EMPTY));
	await expect(feedback).toHaveAttribute('role', 'alert');
	await expectInvalid(input, 'Empty file fails validation and marks the input invalid');

	await input.setInputFiles(pickerFile(VALID));
	await expect(feedback).toHaveAttribute('role', 'status');
	await expect(feedback).toContainText(VALID.name);
	await expectNotInvalid(input, 'Successful validation must not keep the invalid mark');
	const feedbackId = await feedback.getAttribute('id');
	expect(await describedByIds(input), 'Success keeps the hint plus feedback association').toEqual([
		...pristineDescribed, feedbackId as string
	]);

	await page.getByRole('button', { name: /^clear/i }).click();
	await expect(feedback).toHaveCount(0);
	await expect(input).toHaveJSProperty('value', '');
	await expectNotInvalid(input, 'Clear restores pristine invalid-state semantics');
	expect(await describedByIds(input), 'Clear restores the pristine description list').toEqual(pristineDescribed);
});

test('IMPORTRECOVERY-03 picker and drop agree on aria-invalid for invalid and valid files', async ({ page }) => {
	const input = page.getByTestId('import-file');
	const feedback = page.getByTestId('import-status');

	await input.setInputFiles(pickerFile(WRONG_EXT));
	await expect(feedback).toHaveAttribute('role', 'alert');
	await expectInvalid(input, 'Picker rejection marks the input invalid');
	const pickerError = await feedback.innerText();
	await page.getByRole('button', { name: /^clear/i }).click();
	await expectNotInvalid(input, 'Clear between entry points resets the mark');

	await dropFiles(page, [WRONG_EXT]);
	await expect(feedback, 'Drop uses the same validation path as the picker').toHaveAttribute('role', 'alert');
	await expect(feedback).toHaveText(pickerError);
	await expectInvalid(input, 'Drop rejection marks the input invalid');

	await dropFiles(page, [VALID]);
	await expect(feedback).toHaveAttribute('role', 'status');
	await expect(feedback).toContainText(VALID.name);
	await expectNotInvalid(input, 'Valid drop clears the invalid mark');

	await input.setInputFiles(pickerFile(WRONG_EXT));
	await expect(feedback).toHaveAttribute('role', 'alert');
	await expectInvalid(input, 'Picker rejection after a valid drop marks the input invalid again');

	await input.setInputFiles(pickerFile(VALID));
	await expect(feedback).toHaveAttribute('role', 'status');
	await expectNotInvalid(input, 'Valid picker selection clears the invalid mark again');
});

test('IMPORTRECOVERY-04 identical repeated explicit actions refresh one stable feedback region', async ({ page }) => {
	const input = page.getByTestId('import-file');
	const feedback = page.getByTestId('import-status');

	// Picker, valid path: the same file after Clear is a fresh explicit
	// action. The remounted region announces by insertion; text/role pin it.
	await input.setInputFiles(pickerFile(VALID));
	await expect(feedback).toHaveAttribute('role', 'status');
	const firstText = await feedback.innerText();
	await page.getByRole('button', { name: /^clear/i }).click();
	await expect(feedback).toHaveCount(0);
	await input.setInputFiles(pickerFile(VALID));
	await expect(feedback).toHaveAttribute('role', 'status');
	await expect(feedback).toHaveText(firstText);
	await expect(feedback, 'One feedback region serves repeated actions').toHaveCount(1);

	// Picker, invalid path: same bad file after Clear errors identically.
	await input.setInputFiles(pickerFile(WRONG_EXT));
	await expect(feedback).toHaveAttribute('role', 'alert');
	const firstError = await feedback.innerText();
	await page.getByRole('button', { name: /^clear/i }).click();
	await expect(feedback).toHaveCount(0);
	await input.setInputFiles(pickerFile(WRONG_EXT));
	await expect(feedback).toHaveAttribute('role', 'alert');
	await expect(feedback).toHaveText(firstError);
	await expect(feedback, 'Repeated rejection still uses the single feedback region').toHaveCount(1);

	// Consecutive identical drops while the region stays mounted: equal text
	// must not go silent. The same stable element receives at least one
	// fresh non-empty commit, and each explicit drop adds at most one.
	const mounted = await feedback.elementHandle();
	await observeCommits(feedback);
	await dropFiles(page, [WRONG_EXT]);
	await dropFiles(page, [WRONG_EXT]);
	await expect(feedback).toHaveAttribute('role', 'alert');
	await expect(feedback).toHaveText(firstError);
	await expect(feedback, 'Repeated drops still use the single feedback region').toHaveCount(1);
	expect(await feedback.evaluate((element, previous) => element === previous, mounted),
		'Consecutive outcomes update the same stable region element').toBe(true);
	const dropCommits = await commitsOf(feedback);
	const freshErrors = dropCommits.nonEmpty.filter((entry) => entry === firstError).length;
	expect(freshErrors,
		'At least one fresh commit reaches assistive tech for the repeated drops').toBeGreaterThanOrEqual(1);
	expect(dropCommits.nonEmpty.length,
		'Two explicit drops produce at most one outcome commit each').toBeLessThanOrEqual(2);

	// Consecutive identical valid drops behave the same way.
	await dropFiles(page, [VALID]);
	await expect(feedback).toHaveAttribute('role', 'status');
	const validText = await feedback.innerText();
	await observeCommits(feedback);
	await dropFiles(page, [VALID]);
	await expect(feedback).toHaveText(validText);
	await expect(feedback, 'Repeated valid drops keep the single feedback region').toHaveCount(1);
	const validCommits = await commitsOf(feedback);
	expect(validCommits.nonEmpty.filter((entry) => entry === validText).length,
		'The repeated valid drop is freshly committed once').toBe(1);
	expect(validCommits.nonEmpty.length, 'Repeated valid drop adds no redundant commits').toBe(1);
});

test('IMPORTRECOVERY-05 Clear announces one distinct short outcome while file-specific feedback disappears', async ({ page }) => {
	const input = page.getByTestId('import-file');
	const feedback = page.getByTestId('import-status');

	await input.setInputFiles(pickerFile(VALID));
	await expect(feedback).toHaveAttribute('role', 'status');
	await page.getByRole('button', { name: /^clear/i }).click();
	await expect(feedback, 'File-specific import-status feedback disappears on clear').toHaveCount(0);
	await expect(page.getByRole('main').getByRole('alert')).toHaveCount(0);
	const first = await clearOutcome(page);
	await observeCommits(first.locator);
	await observeIdleMutations(page);
	expect(await idleMutations(page), 'Idle rendering never re-announces the cleared outcome').toBe(0);
	expect((await commitsOf(first.locator)).nonEmpty, 'No outcome recommits while idle').toEqual([]);

	// Repeated clear is safe: when a Clear control is still offered with
	// nothing selected, activating it changes neither region nor input.
	const again = page.getByRole('button', { name: /^clear/i });
	if (await again.count()) {
		await again.click();
		await expect(feedback).toHaveCount(0);
		await expect(input).toHaveJSProperty('value', '');
		expect(((await first.locator.textContent()) ?? '').trim(), 'Repeated no-op clear does not swap the outcome').toBe(first.text);
	}
});

test('IMPORTRECOVERY-06 keyboard Clear returns focus to the native input; selections and the chooser never steal focus', async ({ page }) => {
	const input = page.getByTestId('import-file');
	const feedback = page.getByTestId('import-status');
	const clear = page.getByRole('button', { name: /^clear/i });

	await input.setInputFiles(pickerFile(VALID));
	await expect(feedback).toHaveAttribute('role', 'status');
	await clear.focus();
	await expect(clear).toBeFocused();
	await page.keyboard.press('Enter');
	await expect(feedback).toHaveCount(0);
	await expect(input, 'After Clear unmounts, focus lands on the native file input').toBeFocused();

	// Replacement keeps keyboard position: selecting while the input holds
	// focus leaves focus on the input, and the same file re-selects later.
	await input.setInputFiles(pickerFile(WRONG_EXT));
	await expect(feedback).toHaveAttribute('role', 'alert');
	await expect(input, 'Validation replacement does not steal focus').toBeFocused();

	// Cancelling the chooser returns without moving focus off the input.
	await input.evaluate((element) => element.dispatchEvent(new Event('cancel')));
	await page.waitForTimeout(100);
	await expect(input, 'Chooser cancellation keeps focus on the input').toBeFocused();

	await clear.focus();
	await page.keyboard.press('Enter');
	await expect(input, 'Second Clear also recovers focus to the input').toBeFocused();
	await input.setInputFiles(pickerFile(WRONG_EXT));
	await expect(feedback).toHaveAttribute('role', 'alert');
	await expect(input, 'Re-selecting the same rejected file keeps focus usable').toBeFocused();
});

test('IMPORTRECOVERY-07 drag hover leaves focus, validation state and feedback untouched', async ({ page }) => {
	const input = page.getByTestId('import-file');
	const feedback = page.getByTestId('import-status');
	const clear = page.getByRole('button', { name: /^clear/i });

	await input.setInputFiles(pickerFile(VALID));
	await expect(feedback).toHaveAttribute('role', 'status');
	const beforeText = await feedback.innerText();
	const beforeInvalid = await input.getAttribute('aria-invalid');
	await clear.focus();
	await expect(clear).toBeFocused();
	await dragHover(page);
	await expect(clear, 'Drag hover must not steal focus from the focused control').toBeFocused();
	await expect(feedback).toHaveText(beforeText);
	expect(await input.getAttribute('aria-invalid'), 'Hover alone never changes validation state').toBe(beforeInvalid);
});

test('IMPORTRECOVERY-08 hostile filenames stay escaped text on valid and rejected paths', async ({ page }) => {
	const dialogs: string[] = [];
	page.on('dialog', async (dialog) => { dialogs.push(dialog.message()); await dialog.dismiss(); });
	const input = page.getByTestId('import-file');
	const feedback = page.getByTestId('import-status');

	const hostileValid = '<svg onload=alert(1)>.nzb';
	await input.setInputFiles(pickerFile({ name: hostileValid, size: 19 }));
	await expect(feedback).toHaveAttribute('role', 'status');
	await expect(feedback).toContainText(hostileValid);
	await expect(feedback.locator('svg, img, script')).toHaveCount(0);
	await expectNotInvalid(input, 'Hostile but metadata-valid file is not marked invalid');

	const hostileRejected = '<img src=x onerror=alert(1)>.txt';
	await input.setInputFiles(pickerFile({ name: hostileRejected, size: 19 }));
	await expect(feedback).toHaveAttribute('role', 'alert');
	await expect(feedback.locator('svg, img, script')).toHaveCount(0);
	await expectInvalid(input, 'Hostile rejected file still marks the input invalid');
	expect(dialogs).toEqual([]);
});

test.describe('IMPORTRECOVERY-09 narrow-screen recovery', () => {
	test.use({ hasTouch: true, isMobile: true });
	for (const colorScheme of ['light', 'dark'] as const) {
		test(`IMPORTRECOVERY-09-${colorScheme} 320px keeps invalid state, one clear outcome and input focus without overflow`, async ({ page }) => {
			await page.setViewportSize({ width: 320, height: 640 });
			await page.emulateMedia({ colorScheme, reducedMotion: 'reduce' });
			const input = page.getByTestId('import-file');
			const feedback = page.getByTestId('import-status');

			await input.setInputFiles(pickerFile({ name: `${'long-recovery-name-'.repeat(8)}.txt`, size: 37 }));
			await expect(feedback).toHaveAttribute('role', 'alert');
			await expectInvalid(input, 'Narrow-screen rejection marks the input invalid');
			expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(320);

			const clear = page.getByRole('button', { name: /^clear/i });
			await clear.focus();
			await expect(clear).toBeFocused();
			await page.keyboard.press('Enter');
			await expect(feedback).toHaveCount(0);
			await expect(input, 'Narrow-screen Clear returns focus to the input').toBeFocused();
			await expectNotInvalid(input, 'Narrow-screen Clear resets the invalid mark');
			await clearOutcome(page);
			expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(320);
		});
	}
});
