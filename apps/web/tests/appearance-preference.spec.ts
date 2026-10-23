import { expect, test } from '@playwright/test';
import {
	APPEARANCE_KEY, assertClean, assertNativeScheme, assertPalette, captureEvidence, choose,
	openAppearance, selected, stored, type Appearance, type Palette
} from './appearance.helpers';

test.afterEach(async ({ page }, testInfo) => captureEvidence(page, testInfo));

// Protected preference acceptance. All cases exercise real application startup
// and DOM effects, not a replacement preference implementation in the tests.
for (const os of ['light', 'dark'] as const) {
	for (const preference of ['system', 'light', 'dark'] as const) {
		test(`APP-PREF-01-${os}-${preference}: restores a validated enum under ${os} OS`, async ({ page }) => {
			const errors = await openAppearance(page, os, preference);
			const palette = preference === 'system' ? os : preference;
			await assertPalette(page, palette);
			await selected(page, preference);
			await assertNativeScheme(page, palette);
			await assertClean(page, errors);
		});
	}

	const invalid = [
		['missing', undefined], ['empty', ''], ['unknown', 'sepia'], ['case', 'Dark'],
		['padded', ' dark '], ['json-string', '"dark"'], ['json-object', '{"appearance":"dark"}'],
		['null-text', 'null'], ['boolean', 'true'], ['prototype-name', '__proto__'], ['markup', '<script>dark</script>']
	] as const;
	for (const [id, raw] of invalid) {
		test(`APP-PREF-02-${os}-${id}: malformed or missing storage falls back to System`, async ({ page }) => {
			const errors = await openAppearance(page, os, raw);
			await selected(page, 'system');
			await assertPalette(page, os);
			await assertNativeScheme(page, os);
			const other: Palette = os === 'light' ? 'dark' : 'light';
			await page.emulateMedia({ colorScheme: other });
			await assertPalette(page, other);
			await assertNativeScheme(page, other);
			await assertClean(page, errors);
		});
	}
}

for (const refusal of ['getter', 'read', 'write'] as const) {
	test(`APP-PREF-03-${refusal}: storage refusal does not prevent System or in-memory choices`, async ({ page }) => {
		await page.addInitScript((mode) => {
			if (mode === 'getter') {
				Object.defineProperty(window, 'localStorage', {
					configurable: true,
					get() { throw new DOMException('Storage unavailable', 'SecurityError'); }
				});
			} else {
				Storage.prototype[mode === 'read' ? 'getItem' : 'setItem'] = function () {
					throw new DOMException('Storage unavailable', mode === 'read' ? 'SecurityError' : 'QuotaExceededError');
				};
			}
		}, refusal);
		const errors = await openAppearance(page, 'light');
		await selected(page, 'system');
		await assertPalette(page, 'light');
		await page.emulateMedia({ colorScheme: 'dark' });
		await assertPalette(page, 'dark');
		await choose(page, 'light');
		await assertPalette(page, 'light');
		await assertNativeScheme(page, 'light');
		await choose(page, 'system');
		await assertPalette(page, 'dark');
		await page.getByRole('link', { name: 'Create an account', exact: true }).click();
		await expect(page).toHaveURL(/\/signup$/);
		await selected(page, 'system');
		await assertPalette(page, 'dark');
		await page.goBack();
		await expect(page).toHaveURL(/\/login$/);
		await page.reload();
		await selected(page, 'system');
		await assertPalette(page, 'dark');
		await expect(page.getByRole('main')).toBeVisible();
		await assertClean(page, errors);
	});
}

test('APP-PREF-04: each choice persists only its enum and reloads faithfully, including returning to System', async ({ page }) => {
	const errors = await openAppearance(page, 'light');
	for (const preference of ['dark', 'light', 'system'] as Appearance[]) {
		await choose(page, preference);
		expect(await stored(page)).toBe(preference);
		await page.reload();
		await selected(page, preference);
		await assertPalette(page, preference === 'system' ? 'light' : preference);
		await assertNativeScheme(page, preference === 'system' ? 'light' : preference);
	}
	await assertClean(page, errors);
});

test('APP-PREF-05: changing appearance writes no identity, cookie, other preference or backend request', async ({ page }) => {
	const errors = await openAppearance(page, 'light');
	const attempts: string[] = [];
	page.on('request', (request) => {
		const url = new URL(request.url());
		if (['fetch', 'xhr'].includes(request.resourceType()) || request.method() !== 'GET' || /^\/(?:api|v1)(?:\/|$)/.test(url.pathname) || !['localhost', '127.0.0.1'].includes(url.hostname)) {
			attempts.push(`${request.method()} ${url.pathname}`);
		}
	});
	const before = await page.evaluate(() => ({ local: { ...localStorage }, session: { ...sessionStorage }, cookie: document.cookie }));
	await page.evaluate(() => {
		(window as Window & { __appearanceAudit: { writes: [string, string][] } }).__appearanceAudit.writes = [];
	});
	await choose(page, 'dark');
	await choose(page, 'light');
	await choose(page, 'system');
	await page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
	const after = await page.evaluate(() => ({ local: { ...localStorage }, session: { ...sessionStorage }, cookie: document.cookie }));
	const writes = await page.evaluate(() => (window as Window & { __appearanceAudit: { writes: [string, string][] } }).__appearanceAudit.writes);
	expect(writes).toEqual([[APPEARANCE_KEY, 'dark'], [APPEARANCE_KEY, 'light'], [APPEARANCE_KEY, 'system']]);
	expect(after).toEqual({ ...before, local: { ...before.local, [APPEARANCE_KEY]: 'system' } });
	expect(attempts).toEqual([]);
	await assertClean(page, errors);
});
