import { expect, test, type CDPSession, type Locator, type Page } from '@playwright/test';
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

async function mountViewportFixture(page: Page) {
	await page.setContent(`<!doctype html><html lang="en"><head>
		<meta name="viewport" content="width=device-width, initial-scale=1">
		<style>
			body { margin: 0; min-height: 100vh; --nt-space-1: 4px; --nt-space-2: 8px; --nt-space-3: 12px; }
			[role="status"] { margin: 0; }
			[data-testid="fixture-row"] { position: absolute; left: 12px; top: 46px; }
			[data-testid="fixture-row"] button { font: 11px sans-serif; }
			[data-context-menu-longpress] { user-select: none; }
		</style></head><body></body></html>`);
	await page.addScriptTag({ content: script });
}

async function expectInVisualViewport(element: Locator, sheet = false) {
	await expect(element).toBeVisible();
	await expect.poll(() => element.evaluate((node, sheet) => {
		const viewport = window.visualViewport!;
		const rect = node.getBoundingClientRect();
		if (sheet) {
			return Math.max(Math.abs(rect.left - viewport.offsetLeft), Math.abs(rect.width - viewport.width),
				Math.abs(rect.bottom - viewport.offsetTop - viewport.height), viewport.offsetTop - rect.top);
		}
		return Math.max(viewport.offsetLeft - rect.left, viewport.offsetTop - rect.top,
			rect.right - viewport.offsetLeft - viewport.width, rect.bottom - viewport.offsetTop - viewport.height);
	}, sheet)).toBeLessThanOrEqual(1);
}

function viewportGeometry(page: Page) {
	return page.evaluate(() => {
		const viewport = window.visualViewport!;
		return { scale: viewport.scale, x: viewport.offsetLeft, y: viewport.offsetTop,
			width: viewport.width, height: viewport.height, scrollX: window.scrollX, scrollY: window.scrollY };
	});
}

async function touch(session: CDPSession, type: 'touchStart' | 'touchMove' | 'touchEnd', x = 0, y = 0) {
	await session.send('Input.dispatchTouchEvent', { type, touchPoints: type === 'touchEnd' ? [] : [{ x, y }] });
}

async function panVisualViewport(page: Page, session: CDPSession) {
	// Start above these menus so the gesture pans the page, not a menu scrollport.
	await touch(session, 'touchStart', 172, 38);
	for (let step = 1; step <= 7; step++) {
		await touch(session, 'touchMove', 172 - step * 5, 38 - step * 5);
	}
	await touch(session, 'touchEnd');
	await expect.poll(async () => {
		const viewport = await viewportGeometry(page);
		return { ...viewport, panned: viewport.x > 10 && viewport.y > 10 };
	}).toMatchObject({ scale: 2, panned: true, scrollX: 0, scrollY: 0 });
}

for (const scale of [1, 2]) {
	test(`fine-pointer menus fit the visual viewport at ${scale}× page scale`, async ({ page }) => {
		await page.setViewportSize({ width: 390, height: 420 });
		await mountViewportFixture(page);
		const session = await page.context().newCDPSession(page);
		await session.send('Emulation.setPageScaleFactor', { pageScaleFactor: scale });
		await expect.poll(() => viewportGeometry(page)).toMatchObject({ scale, width: 390 / scale, height: 420 / scale });
		expect(await page.evaluate(() => matchMedia('(pointer: fine)').matches)).toBe(true);

		const more = page.getByRole('button', { name: 'Open fixture menu' });
		const play = page.getByRole('button', { name: 'Play fixture' });
		const root = page.getByRole('menu', { name: 'Fixture actions', exact: true });
		const child = page.getByRole('menu', { name: 'Destinations', exact: true });
		await expectInVisualViewport(more);
		await more.click();
		await expectInVisualViewport(root);
		await page.keyboard.press('Escape');
		await expect(more).toBeFocused();

		await play.click({ button: 'right' });
		await expectInVisualViewport(root);
		if (scale === 2) {
			// Shrink the visual viewport alone so End has to scroll the root.
			await session.send('Emulation.setPageScaleFactor', { pageScaleFactor: 3 });
			await expect.poll(() => viewportGeometry(page)).toMatchObject({ scale: 3, height: 140 });
			await expectInVisualViewport(root);
		}
		await page.keyboard.press('End');
		const parent = root.getByRole('menuitem', { name: 'Destinations', exact: true });
		await expect(parent).toBeFocused();
		if (scale === 2) expect(await root.evaluate((node) => node.scrollTop)).toBeGreaterThan(0);
		await page.keyboard.press('ArrowRight');
		await expectInVisualViewport(child);
		if (scale === 2) {
			await session.send('Emulation.setPageScaleFactor', { pageScaleFactor: 2 });
			await expect.poll(() => viewportGeometry(page)).toMatchObject({ scale: 2, width: 195, height: 210 });
			await expectInVisualViewport(root);
			await expectInVisualViewport(child);
			await panVisualViewport(page, session);
			await expectInVisualViewport(root);
			await expectInVisualViewport(child);
		}
		await page.setViewportSize({ width: 342, height: 300 });
		await expect.poll(() => viewportGeometry(page)).toMatchObject({ scale, width: 342 / scale, height: 300 / scale });
		await expectInVisualViewport(root);
		await expectInVisualViewport(child);
		await page.keyboard.press('ArrowLeft');
		await expect(child).toHaveCount(0);
		await expect(parent).toBeFocused();
		await expect(parent).toBeInViewport({ ratio: 1 });
		await page.keyboard.press('ArrowRight');
		if (scale === 2) {
			await expectInVisualViewport(child);
			await root.evaluate((node) => {
				node.scrollTop = 0;
				// Deliver a viewport update ahead of the queued menu scroll event.
				window.visualViewport!.dispatchEvent(new Event('scroll'));
			});
			await expect(child).toHaveCount(0);
			await expect(parent).toBeFocused();
			await page.keyboard.press('End');
			await page.keyboard.press('ArrowRight');
		}
		await page.keyboard.press('End');
		const action = child.getByRole('menuitem', { name: 'Second destination' });
		await expect(action).toBeFocused();
		await expectInVisualViewport(action);
		const point = await action.evaluate((node) => {
			const rect = node.getBoundingClientRect();
			const x = (rect.left + rect.right) / 2;
			const y = (rect.top + rect.bottom) / 2;
			// CDP pointer input uses the visual origin after a page-scale pan.
			return { x: x - window.visualViewport!.offsetLeft, y: y - window.visualViewport!.offsetTop,
				hit: node.contains(document.elementFromPoint(x, y)) };
		});
		expect(point.hit).toBe(true);
		await page.mouse.click(point.x, point.y);
		await expect(page.getByRole('status')).toHaveText('Second activated');
		await expect(page.getByRole('menu')).toHaveCount(0);
		await expect(more).toBeFocused();
	});

	test(`coarse-pointer sheets fit the visual viewport at ${scale}× page scale`, async ({ browser }) => {
		const context = await browser.newContext({ viewport: { width: 390, height: 420 }, isMobile: true, hasTouch: true });
		try {
			const page = await context.newPage();
			await mountViewportFixture(page);
			const session = await context.newCDPSession(page);
			await session.send('Emulation.setPageScaleFactor', { pageScaleFactor: scale });
			await expect.poll(() => viewportGeometry(page)).toMatchObject({ scale, width: 390 / scale, height: 420 / scale });
			expect(await page.evaluate(() => matchMedia('(pointer: coarse)').matches)).toBe(true);

			const more = page.getByRole('button', { name: 'Open fixture menu' });
			const play = page.getByRole('button', { name: 'Play fixture' });
			const root = page.getByRole('menu', { name: 'Fixture actions', exact: true });
			const child = page.getByRole('menu', { name: 'Destinations', exact: true });
			await expectInVisualViewport(more);
			await more.tap();
			await expectInVisualViewport(root, true);
			await page.touchscreen.tap(10, 10);
			await expect(root).toHaveCount(0);
			await expect(more).toBeFocused();

			await play.evaluate((button) => {
				document.addEventListener('click', (event) => {
					if (event.detail && event.composedPath().includes(button)) button.setAttribute('data-release-click', '');
				}, true);
			});
			const box = (await play.boundingBox())!;
			await touch(session, 'touchStart', box.x + box.width / 2, box.y + box.height / 2);
			await expect(root).toBeVisible();
			await touch(session, 'touchEnd');
			// Chromium can omit the compatibility click after a long press.
			if (await play.getAttribute('data-release-click') === null) await play.dispatchEvent('click', { detail: 1 });
			await expect(page.getByRole('status')).toBeEmpty();
			await expectInVisualViewport(root, true);
			const parent = root.getByRole('menuitem', { name: 'Destinations', exact: true });
			await page.keyboard.press('End');
			await expectInVisualViewport(parent);
			await parent.tap();
			await expectInVisualViewport(child, true);
			if (scale === 2) {
				await panVisualViewport(page, session);
				await expectInVisualViewport(child, true);
			}
			await page.setViewportSize({ width: 342, height: 300 });
			await expect.poll(() => viewportGeometry(page)).toMatchObject({ scale, width: 342 / scale, height: 300 / scale });
			await expectInVisualViewport(child, true);
			if (scale === 1) {
				await page.setViewportSize({ width: 780, height: 300 });
				expect(await page.evaluate(() => matchMedia('(max-width: 767px)').matches)).toBe(false);
				await expectInVisualViewport(child, true);
			}
			await child.getByRole('menuitem', { name: 'Back', exact: true }).tap();
			await expect(parent).toBeFocused();
			await expectInVisualViewport(root, true);
			await parent.tap();
			await page.keyboard.press('End');
			const action = child.getByRole('menuitem', { name: 'Second destination' });
			await expectInVisualViewport(action);
			await action.tap();
			await expect(page.getByRole('status')).toHaveText('Second activated');
			await expect(page.getByRole('menu')).toHaveCount(0);
			await expect(more).toBeFocused();
		} finally {
			await context.close();
		}
	});
}
