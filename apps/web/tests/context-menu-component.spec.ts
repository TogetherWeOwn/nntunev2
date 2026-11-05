import { expect, test } from '@playwright/test';
import { build } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { fileURLToPath } from 'node:url';

// Bundle the real component into a test-only page. No fixture route or bundle
// is included in the product build, and no DOM emulator replaces popover/focus.
let script: string;
test.beforeAll(async () => {
	const result = await build({
		configFile: false,
		root: fileURLToPath(new URL('../', import.meta.url)),
		logLevel: 'error',
		plugins: [svelte({ configFile: false, emitCss: false, compilerOptions: { css: 'injected' } })],
		build: {
			write: false,
			minify: false,
			lib: { entry: fileURLToPath(new URL('./fixtures/menu-harness.ts', import.meta.url)), name: 'MenuFixture', formats: ['iife'] }
		}
	});
	const bundles = Array.isArray(result) ? result : [result];
	const chunk = bundles.flatMap((bundle) => 'output' in bundle ? bundle.output : []).find((entry) => entry.type === 'chunk');
	if (!chunk || chunk.type !== 'chunk') throw new Error('Missing fixture script');
	script = chunk.code;
});

test('desktop multi-choice flyout, one-choice flattening and focus restoration', async ({ page }) => {
	await page.setContent('<!doctype html><html lang="en"><head><meta name="viewport" content="width=device-width, initial-scale=1"></head><body></body></html>');
	await page.addScriptTag({ content: script });
	const invoker = page.getByRole('button', { name: 'Open fixture menu' });
	await invoker.click();
	await expect(page.getByRole('menuitem', { name: 'Inline action', exact: true })).toBeFocused();
	await expect(page.getByRole('menuitem', { name: 'One choice', exact: true })).toHaveCount(0);
	await page.keyboard.press('End');
	const parent = page.getByRole('menuitem', { name: 'Destinations', exact: true });
	await expect(parent).toBeFocused();
	await page.keyboard.press('ArrowRight');
	await expect(page.getByRole('menu')).toHaveCount(2);
	await expect(page.getByRole('menuitem', { name: 'First destination' })).toBeFocused();
	await page.keyboard.press('ArrowLeft');
	await expect(page.getByRole('menu')).toHaveCount(1);
	await expect(parent).toBeFocused();
	await page.keyboard.press('ArrowRight');
	await page.keyboard.press('End');
	await page.keyboard.press('Enter');
	await expect(page.getByRole('status')).toHaveText('Second activated');
	await expect(page.getByRole('menu')).toHaveCount(0);
	await expect(invoker).toBeFocused();
});

test('coarse mobile submenu replaces the sheet and Back restores its parent item', async ({ browser }) => {
	const context = await browser.newContext({ viewport: { width: 390, height: 700 }, isMobile: true, hasTouch: true });
	const page = await context.newPage();
	await page.setContent('<!doctype html><html lang="en"><head><meta name="viewport" content="width=device-width, initial-scale=1"></head><body></body></html>');
	await page.addScriptTag({ content: script });
	await page.getByRole('button', { name: 'Open fixture menu' }).tap();
	await page.getByRole('menuitem', { name: 'Destinations', exact: true }).tap();
	await expect(page.getByRole('menu')).toHaveCount(1);
	await expect(page.getByRole('menu', { name: 'Destinations', exact: true })).toBeVisible();
	await expect(page.getByRole('menuitem', { name: 'Back', exact: true })).toBeFocused();
	await expect(page.getByRole('menuitem', { name: 'Inline action', exact: true })).toHaveCount(0);
	await page.getByRole('menuitem', { name: 'Back', exact: true }).tap();
	await expect(page.getByRole('menuitem', { name: 'Destinations', exact: true })).toBeFocused();
	await page.keyboard.press('Enter');
	await page.keyboard.press('Escape');
	await expect(page.getByRole('menuitem', { name: 'Destinations', exact: true })).toBeFocused();
	await page.keyboard.press('Escape');
	await expect(page.getByRole('menu')).toHaveCount(0);
	await expect(page.getByRole('button', { name: 'Open fixture menu' })).toBeFocused();
	await context.close();
});

test('desktop flyout child removed by hovering a sibling leaves focus on the surviving parent', async ({ page }) => {
	await page.setContent('<!doctype html><html lang="en"><head><meta name="viewport" content="width=device-width, initial-scale=1"></head><body></body></html>');
	await page.addScriptTag({ content: script });
	const invoker = page.getByRole('button', { name: 'Open fixture menu' });
	await invoker.click();
	await page.keyboard.press('End');
	const parent = page.getByRole('menuitem', { name: 'Destinations', exact: true });
	await expect(parent).toBeFocused();
	await page.keyboard.press('ArrowRight');
	await expect(page.getByRole('menu')).toHaveCount(2);
	await expect(page.getByRole('menuitem', { name: 'First destination' })).toBeFocused();
	await page.getByRole('menuitem', { name: 'Inline action', exact: true }).hover();
	await expect(page.getByRole('menu')).toHaveCount(1);
	await expect(page.locator('body')).not.toBeFocused();
	await expect(parent).toBeFocused();
	await page.keyboard.press('Escape');
	await expect(page.getByRole('menu')).toHaveCount(0);
	await expect(invoker).toBeFocused();
});
