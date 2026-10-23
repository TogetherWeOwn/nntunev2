import { existsSync, readFileSync } from 'node:fs';

import { expect, test, type Page } from '@playwright/test';

import { MAX_NZB_IMPORT_BYTES } from '../src/lib/library/import-validation.js';

/**
 * Acceptance: truthful local NZB selection feedback (import echo bundle).
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build lands the route
 * changes on top of this branch and must make these tests pass WITHOUT
 * editing this file; a wrong test comes back to the acceptance card instead.
 * The unit half is `src/lib/library/import-echo-acceptance.test.ts` (single
 * size-cap source pins).
 *
 * Stable case IDs:
 * - IMPORTECHO-00 suite hygiene: this spec reuses the single exported cap
 *   for its boundary buffers (no byte literals of its own).
 * - IMPORTECHO-01 valid selection echoes file name + human-readable size
 *   with local-only disclosure (garbage content still validates:
 *   metadata-only, no content parse).
 * - IMPORTECHO-02 empty file keeps its typed error with no success text.
 * - IMPORTECHO-03 at-cap file validates; one byte over keeps the too-large
 *   error (boundaries driven by the imported single cap, no literals here).
 * - IMPORTECHO-04 wrong extension keeps its typed error with no success text.
 * - IMPORTECHO-05 static requirements hint states the .nzb, non-empty and
 *   2 MiB-ceiling rules before any selection and persists after selection.
 * - IMPORTECHO-06 Clear resets the input + feedback to pristine
 *   (keyboard-operable, labeled, no-op-safe, hint survives).
 * - IMPORTECHO-07 reselecting the same file after Clear re-validates
 *   (valid and invalid paths).
 * - IMPORTECHO-08 feedback swaps status/alert exclusively and the input
 *   stays described by the feedback element while it is shown.
 *
 * Contract notes for the builder (exact wording is yours; topics are pinned):
 * - Existing hooks stay: `import-file` (file input, `accept=".nzb"`),
 *   `import-status` (`role=status` valid / `role=alert` invalid).
 * - Clear is found by role, not by test id: a button whose accessible name
 *   starts with "Clear" (e.g. "Clear", "Clear selection"). It must be
 *   focusable and activatable from the keyboard, and activating it with
 *   nothing selected must be a safe no-op (hiding it while pristine is
 *   fine). No new test id is required.
 * - The hint is found by its static text topics (`.nzb`, non-empty,
 *   2 MiB ceiling as a limit), present before any selection. It may live in
 *   the import route or a shared component.
 * - The size echo accepts any honest human-readable rendering of the
 *   validated `File` bytes ("184 KiB", "188,416 bytes", "0.18 MiB", ...).
 * - `aria-describedby` must include the feedback element id while feedback
 *   is shown and must not reference it once cleared; adding the hint id to
 *   the describedby list is allowed.
 *
 * Determinism: synthetic in-memory buffers only (no real NZB, no fixtures
 * on disk, no credentials, no backend); service workers blocked; every case
 * asserts zero network requests after page load. Local build + preview only.
 *
 * Focused command (repo-local, deterministic, offline once installed):
 *   pnpm --dir apps/web exec playwright test tests/import-echo.spec.ts
 */

// Boundary buffers below derive from the single exported cap — this spec
// carries no byte literal of its own (self-pinned by IMPORTECHO-00).
const AT_CAP = MAX_NZB_IMPORT_BYTES;
const OVER_CAP = MAX_NZB_IMPORT_BYTES + 1;

// Any honest human-readable rendering of a byte count.
const HUMAN_SIZE = /\d[\d,]*(?:\.\d+)?\s?(?:bytes?|B|KB|KiB|MB|MiB)\b/;
// Unambiguous success claims: must never appear in an error.
const SUCCESS_WORDS = /\b(?:success(?:ful)?|ready|complete(?:d)?|imported|uploaded)\b/i;

test.use({ serviceWorkers: 'block' });

async function gotoQuiet(page: Page): Promise<string[]> {
	await page.goto('/library/import');
	await page.waitForLoadState('networkidle');
	const requests: string[] = [];
	page.on('request', (request) => requests.push(`${request.method()} ${request.url()}`));
	return requests;
}

function expectLocalOnlySelection(copy: string): void {
	expect(copy, 'status reports a selection').toMatch(/\bselected\b/i);
	expect(copy, 'status keeps the local-only disclosure').toMatch(/\blocal(?:ly)?\b/i);
	const negation = String.raw`\b(?:no|not|never|without|doesn['’]t|isn['’]t|won['’]t)\b[^.!?;]*`;
	expect(copy, 'status discloses no upload').toMatch(
		new RegExp(`${negation}\\bupload(?:s|ed|ing)?\\b`, 'i')
	);
	expect(copy, 'status discloses no processing').toMatch(
		new RegExp(`${negation}\\bprocess(?:es|ed|ing)?\\b`, 'i')
	);
}

async function expectDescribedByFeedback(page: Page, present: boolean): Promise<void> {
	const describedby =
		(await page.getByTestId('import-file').getAttribute('aria-describedby')) ?? '';
	if (present) {
		expect(describedby, 'input is described by the feedback element').toMatch(/import-feedback/);
	} else {
		expect(
			describedby,
			'cleared input is not described by the feedback element'
		).not.toMatch(/import-feedback/);
	}
}

test('IMPORTECHO-00 spec reuses the single cap source for its boundary buffers', () => {
	// Self-read: Playwright specs run under Node with the config dir
	// (`apps/web`) as the working directory; the repo-root form keeps this
	// green when the suite is launched from one level up.
	const candidates = ['tests/import-echo.spec.ts', 'apps/web/tests/import-echo.spec.ts'];
	const selfPath = candidates.find((candidate) => existsSync(candidate));
	expect(selfPath, 'expected to locate this spec file on disk').toBeDefined();
	const self = readFileSync(selfPath as string, 'utf8');
	expect(self, 'spec imports the single exported cap').toContain('MAX_NZB_IMPORT_BYTES');
	// Built in parts so the guard itself never contains the forbidden literal.
	const decimalFork = new RegExp(['2097', '152'].join(''));
	expect(self, 'spec carries no decimal byte literal').not.toMatch(decimalFork);
	expect(self, 'spec carries no arithmetic byte literal').not.toMatch(
		/2\s*\*\s*1024\s*\*\s*1024/
	);
});

test('IMPORTECHO-01 valid selection echoes file name and human-readable size with local-only disclosure', async ({
	page
}) => {
	const requests = await gotoQuiet(page);
	await page.getByTestId('import-file').setInputFiles({
		name: 'song.nzb',
		mimeType: 'application/x-nzb',
		buffer: Buffer.alloc(188_416)
	});

	const status = page.getByTestId('import-status');
	await expect(status).toBeVisible();
	await expect(status).toHaveAttribute('role', 'status');
	const copy = await status.innerText();
	expect(copy, 'status echoes the selected file name').toContain('song.nzb');
	expect(copy, 'status echoes a human-readable size').toMatch(HUMAN_SIZE);
	expectLocalOnlySelection(copy);
	await expect(page.getByRole('alert')).toHaveCount(0);

	// Metadata-only: non-NZB bytes with a valid name/size still validate —
	// the page must not parse file content.
	await page.getByTestId('import-file').setInputFiles({
		name: 'binary-blob.nzb',
		mimeType: 'application/octet-stream',
		buffer: Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])
	});
	await expect(status).toHaveAttribute('role', 'status');
	await expect(status).toContainText('binary-blob.nzb');
	await expect(page.getByRole('alert')).toHaveCount(0);

	expect(requests, 'selection sends no requests').toEqual([]);
});

test('IMPORTECHO-02 empty file keeps its typed error with no success text', async ({ page }) => {
	const requests = await gotoQuiet(page);
	await page.getByTestId('import-file').setInputFiles({
		name: 'empty.nzb',
		mimeType: 'application/x-nzb',
		buffer: Buffer.alloc(0)
	});

	const feedback = page.getByTestId('import-status');
	await expect(feedback).toBeVisible();
	await expect(feedback).toHaveAttribute('role', 'alert');
	await expect(feedback).toContainText(/empty|no data|0 bytes/i);
	expect(
		await feedback.innerText(),
		'empty-file error adds no success text'
	).not.toMatch(SUCCESS_WORDS);
	await expect(page.getByRole('status')).toHaveCount(0);

	expect(requests, 'rejection sends no requests').toEqual([]);
});

test('IMPORTECHO-03 at-cap file validates while one byte over keeps the too-large error', async ({
	page
}) => {
	const requests = await gotoQuiet(page);
	const input = page.getByTestId('import-file');
	const feedback = page.getByTestId('import-status');

	await input.setInputFiles({
		name: 'at-cap.nzb',
		mimeType: 'application/x-nzb',
		buffer: Buffer.alloc(AT_CAP)
	});
	await expect(feedback).toHaveAttribute('role', 'status');
	await expect(feedback).toContainText('at-cap.nzb');

	await input.setInputFiles({
		name: 'over-cap.nzb',
		mimeType: 'application/x-nzb',
		buffer: Buffer.alloc(OVER_CAP)
	});
	await expect(feedback).toHaveAttribute('role', 'alert');
	await expect(feedback).toContainText(
		/too (large|big)|size|limit|exceed|maximum|2\s?(mb|mib)/i
	);
	expect(
		await feedback.innerText(),
		'too-large error adds no success text'
	).not.toMatch(SUCCESS_WORDS);
	await expect(page.getByRole('status')).toHaveCount(0);

	expect(requests, 'boundary selections send no requests').toEqual([]);
});

test('IMPORTECHO-04 wrong extension keeps its typed error with no success text', async ({
	page
}) => {
	const requests = await gotoQuiet(page);
	await page.getByTestId('import-file').setInputFiles({
		name: 'notes.txt',
		mimeType: 'text/plain',
		buffer: Buffer.from('notes')
	});

	const feedback = page.getByTestId('import-status');
	await expect(feedback).toBeVisible();
	await expect(feedback).toHaveAttribute('role', 'alert');
	await expect(feedback).toContainText(/nzb|extension|type/i);
	expect(
		await feedback.innerText(),
		'extension error adds no success text'
	).not.toMatch(SUCCESS_WORDS);
	await expect(page.getByRole('status')).toHaveCount(0);

	expect(requests, 'rejection sends no requests').toEqual([]);
});

test('IMPORTECHO-05 static requirements hint states the .nzb, non-empty and 2 MiB ceiling rules', async ({
	page
}) => {
	const requests = await gotoQuiet(page);
	await expect(page.getByTestId('import-status')).toHaveCount(0);

	const pristine = await page.getByRole('main').innerText();
	expect(pristine, 'hint names the .nzb extension rule').toMatch(/\.nzb\b/i);
	expect(pristine, 'hint names the non-empty rule').toMatch(
		/non-empty|not empty|with data|must (?:contain|have)/i
	);
	expect(pristine, 'hint names the 2 MiB ceiling').toMatch(/2\s?mib/i);
	expect(pristine, 'hint states the ceiling as a limit').toMatch(
		/or less|≤|<=|max(?:imum)?|up to|no larger than/i
	);

	// Static: the hint persists after a selection.
	await page.getByTestId('import-file').setInputFiles({
		name: 'song.nzb',
		mimeType: 'application/x-nzb',
		buffer: Buffer.alloc(1024)
	});
	await expect(page.getByTestId('import-status')).toBeVisible();
	const selected = await page.getByRole('main').innerText();
	expect(selected, 'hint persists after selection').toMatch(/\.nzb\b/i);
	expect(selected, 'hint persists after selection').toMatch(/2\s?mib/i);

	expect(requests, 'hint and selection send no requests').toEqual([]);
});

test('IMPORTECHO-06 Clear resets the input and feedback to pristine', async ({ page }) => {
	const requests = await gotoQuiet(page);
	const input = page.getByTestId('import-file');
	await input.setInputFiles({
		name: 'song.nzb',
		mimeType: 'application/x-nzb',
		buffer: Buffer.alloc(1024)
	});
	await expect(page.getByTestId('import-status')).toBeVisible();

	const clear = page.getByRole('button', { name: /^clear/i });
	await expect(clear, 'labeled Clear control is available after selection').toBeVisible();
	// Keyboard-reachable: focus and activate without a pointer.
	await clear.focus();
	await page.keyboard.press('Enter');

	await expect(input, 'Clear empties the file input').toHaveJSProperty('value', '');
	await expect(
		page.getByTestId('import-status'),
		'Clear removes the feedback element'
	).toHaveCount(0);
	await expect(page.getByRole('status')).toHaveCount(0);
	await expect(page.getByRole('alert')).toHaveCount(0);
	await expectDescribedByFeedback(page, false);

	// The static hint survives clearing.
	const cleared = await page.getByRole('main').innerText();
	expect(cleared, 'hint persists after clearing').toMatch(/\.nzb\b/i);
	expect(cleared, 'hint persists after clearing').toMatch(/2\s?mib/i);

	// No-op-safe: when Clear is still offered with nothing selected,
	// activating it changes nothing.
	if (await clear.isVisible()) {
		await clear.click();
		await expect(input).toHaveJSProperty('value', '');
		await expect(page.getByTestId('import-status')).toHaveCount(0);
	}

	expect(requests, 'clearing sends no requests').toEqual([]);
});

test('IMPORTECHO-07 reselecting the same file after Clear re-validates', async ({ page }) => {
	const requests = await gotoQuiet(page);
	const input = page.getByTestId('import-file');
	const feedback = page.getByTestId('import-status');
	const clear = page.getByRole('button', { name: /^clear/i });

	const valid = {
		name: 'song.nzb',
		mimeType: 'application/x-nzb',
		buffer: Buffer.alloc(4096)
	};
	await input.setInputFiles(valid);
	await expect(feedback).toHaveAttribute('role', 'status');
	await expect(clear).toBeVisible();
	await clear.click();
	await expect(feedback).toHaveCount(0);
	// Same file again: the cleared input must fire change and re-validate.
	await input.setInputFiles(valid);
	await expect(input).not.toHaveJSProperty('value', '');
	await expect(feedback).toHaveAttribute('role', 'status');
	await expect(feedback).toContainText('song.nzb');

	// Same for the invalid path: the same bad file errors again.
	const invalid = {
		name: 'notes.txt',
		mimeType: 'text/plain',
		buffer: Buffer.from('notes')
	};
	await input.setInputFiles(invalid);
	await expect(feedback).toHaveAttribute('role', 'alert');
	await clear.click();
	await expect(feedback).toHaveCount(0);
	await input.setInputFiles(invalid);
	await expect(feedback).toHaveAttribute('role', 'alert');
	await expect(feedback).toContainText(/nzb|extension|type/i);

	expect(requests, 'reselection sends no requests').toEqual([]);
});

test('IMPORTECHO-08 feedback swaps status and alert exclusively and keeps the input described', async ({
	page
}) => {
	const requests = await gotoQuiet(page);
	const input = page.getByTestId('import-file');
	const feedback = page.getByTestId('import-status');

	await input.setInputFiles({
		name: 'song.nzb',
		mimeType: 'application/x-nzb',
		buffer: Buffer.alloc(1024)
	});
	await expect(feedback).toHaveAttribute('role', 'status');
	await expect(page.getByRole('alert')).toHaveCount(0);
	await expectDescribedByFeedback(page, true);

	await input.setInputFiles({
		name: 'notes.txt',
		mimeType: 'text/plain',
		buffer: Buffer.from('notes')
	});
	await expect(feedback).toHaveAttribute('role', 'alert');
	await expect(page.getByRole('status')).toHaveCount(0);
	await expectDescribedByFeedback(page, true);

	const clear = page.getByRole('button', { name: /^clear/i });
	await expect(clear).toBeVisible();
	await clear.click();
	await expect(feedback).toHaveCount(0);
	await expect(page.getByRole('status')).toHaveCount(0);
	await expect(page.getByRole('alert')).toHaveCount(0);
	await expectDescribedByFeedback(page, false);

	expect(requests, 'feedback transitions send no requests').toEqual([]);
});
