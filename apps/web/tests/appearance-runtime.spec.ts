import { expect, test } from '@playwright/test';
import {
	assertClean, assertNativeScheme, assertPalette, captureEvidence, choose, openAppearance,
	selected, selector, type Appearance, type Palette
} from './appearance.helpers';

test.afterEach(async ({ page }, testInfo) => captureEvidence(page, testInfo));

for (const os of ['light', 'dark'] as const) {
	test(`APP-DOM-01-${os}: the existing layout has a labeled selector and truthful selected state`, async ({ page }) => {
		const errors = await openAppearance(page, os);
		await expect(selector(page)).toBeVisible({ timeout: 2000 });
		await expect(page.getByText('Appearance', { exact: true })).toBeVisible();
		await expect(selector(page).getByRole('option')).toHaveText(['System', 'Light', 'Dark']);
		await selected(page, 'system');
		await assertPalette(page, os);
		await assertNativeScheme(page, os);
		expect(new URL(page.url()).pathname).toBe('/login');
		await assertClean(page, errors);
	});

	for (const preference of ['system', 'light', 'dark'] as const) {
		test(`APP-DOM-02-${os}-${preference}: choice overrides OS only when explicit`, async ({ page }) => {
			const errors = await openAppearance(page, os);
			await choose(page, preference);
			await assertPalette(page, preference === 'system' ? os : preference);
			await assertNativeScheme(page, preference === 'system' ? os : preference);
			const opposite: Palette = os === 'light' ? 'dark' : 'light';
			await page.emulateMedia({ colorScheme: opposite });
			await selected(page, preference);
			await assertPalette(page, preference === 'system' ? opposite : preference);
			await assertNativeScheme(page, preference === 'system' ? opposite : preference);
			await assertClean(page, errors);
		});
	}

	test(`APP-A11Y-01-${os}: Tab focus is visible and native keyboard selection changes the palette`, async ({ page }, testInfo) => {
		const errors = await openAppearance(page, os);
		await expect(selector(page)).toBeVisible({ timeout: 2000 });
		// Reach the control through the actual tab order, not element.focus().
		for (let i = 0; i < 30 && !(await selector(page).evaluate((element) => element === document.activeElement)); i++) {
			await page.keyboard.press('Tab');
		}
		await expect(selector(page)).toBeFocused();
		expect(await selector(page).evaluate((element) => element.matches(':focus-visible'))).toBe(true);
		const focus = await selector(page).evaluate((element) => {
			const css = getComputedStyle(element);
			const luminance = (color: string) => {
				const channels = (color.match(/[\d.]+/g) ?? []).slice(0, 3).map(Number).map((channel) => {
					const value = channel / 255;
					return value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
				});
				return channels[0] * 0.2126 + channels[1] * 0.7152 + channels[2] * 0.0722;
			};
			const outline = luminance(css.outlineColor);
			const backdrop = luminance(getComputedStyle(document.body).backgroundColor);
			return { style: css.outlineStyle, width: parseFloat(css.outlineWidth), color: css.outlineColor,
				contrast: (Math.max(outline, backdrop) + 0.05) / (Math.min(outline, backdrop) + 0.05) };
		});
		expect(focus.style).not.toBe('none');
		expect(focus.width).toBeGreaterThanOrEqual(2);
		expect(focus.color).not.toBe('rgba(0, 0, 0, 0)');
		expect(focus.contrast, 'focus ring must remain visible against the themed page').toBeGreaterThanOrEqual(3);
		await page.screenshot({ path: testInfo.outputPath(`appearance-focus-${os}.png`) });
		await page.keyboard.press('End');
		await page.keyboard.press('Enter');
		await selected(page, 'dark');
		await assertPalette(page, 'dark');
		await page.keyboard.press('Home');
		await page.keyboard.press('ArrowDown');
		await page.keyboard.press('Enter');
		await selected(page, 'light');
		await assertPalette(page, 'light');
		await page.keyboard.press('Home');
		await page.keyboard.press('Enter');
		await selected(page, 'system');
		await assertPalette(page, os);
		await assertClean(page, errors);
	});
}

for (const saved of ['light', 'dark'] as const) {
	test(`APP-PAINT-01-${saved}: saved override is correct at the first revealed application frame under strict CSP`, async ({ page }, testInfo) => {
		const os = saved === 'light' ? 'dark' : 'light';
		const errors = await openAppearance(page, os, saved);
		await expect.poll(() => page.evaluate(() => (window as Window & { __appearanceAudit: { frames: string[] } }).__appearanceAudit.frames.length)).toBeGreaterThan(0);
		const frames = await page.evaluate(() => (window as Window & { __appearanceAudit: { frames: string[] } }).__appearanceAudit.frames);
		await testInfo.attach('first-revealed-frames', { body: JSON.stringify({ os, saved, frames }, null, 2), contentType: 'application/json' });
		await page.screenshot({ path: testInfo.outputPath(`appearance-first-frame-${saved}.png`) });
		expect(frames[0], 'must initialize the saved override before revealing themed content; do not relax CSP').toBe(saved === 'light' ? '#f7f8fb' : '#0b0d12');
		expect(new Set(frames)).toEqual(new Set([saved === 'light' ? '#f7f8fb' : '#0b0d12']));
		await assertPalette(page, saved);
		await assertClean(page, errors);
	});
}

test('APP-LIFE-01: System switches live; route navigation and repeated choices do not accumulate media listeners', async ({ page }) => {
	await page.addInitScript(() => {
		const media = window.matchMedia.bind(window);
		const active = new Map<MediaQueryList, Set<unknown>>();
		window.matchMedia = (query) => {
			const result = media(query);
			if (!query.includes('prefers-color-scheme')) return result;
			const listeners = new Set<unknown>();
			active.set(result, listeners);
			const add = result.addEventListener.bind(result);
			const remove = result.removeEventListener.bind(result);
			result.addEventListener = (type, listener, options) => {
				if (type === 'change' && listener) listeners.add(listener);
				add(type, listener, options);
			};
			result.removeEventListener = (type, listener, options) => {
				if (type === 'change') listeners.delete(listener);
				remove(type, listener, options);
			};
			const addLegacy = result.addListener.bind(result);
			const removeLegacy = result.removeListener.bind(result);
			result.addListener = (listener) => { if (listener) listeners.add(listener); addLegacy(listener); };
			result.removeListener = (listener) => { listeners.delete(listener); removeLegacy(listener); };
			const descriptor = Object.getOwnPropertyDescriptor(MediaQueryList.prototype, 'onchange');
			let onchange: unknown;
			Object.defineProperty(result, 'onchange', {
				configurable: true,
				get() { return descriptor?.get?.call(result); },
				set(listener) {
					listeners.delete(onchange);
					onchange = listener;
					if (listener) listeners.add(listener);
					descriptor?.set?.call(result, listener);
				}
			});
			return result;
		};
		(window as Window & { __appearanceListenerCount?: () => number }).__appearanceListenerCount = () =>
			[...active.values()].reduce((total, listeners) => total + listeners.size, 0);
	});
	const errors = await openAppearance(page, 'light');
	await selected(page, 'system');
	const count = () => page.evaluate(() => (window as Window & { __appearanceListenerCount: () => number }).__appearanceListenerCount());
	const initialCount = await count();
	const documentId = await page.evaluate(() => (window as Window & { __appearanceAudit: { documentId: string } }).__appearanceAudit.documentId);
	for (const os of ['dark', 'light', 'dark'] as const) {
		await page.emulateMedia({ colorScheme: os });
		await assertPalette(page, os);
		await assertNativeScheme(page, os);
	}
	// Login's real internal link starts client-side navigation. Subsequent
	// account links are deliberately not needed: no authenticated backend here.
	for (let i = 0; i < 3; i++) {
		await choose(page, 'light');
		await page.getByRole('link', { name: 'Create an account', exact: true }).click();
		await expect(page).toHaveURL(/\/signup$/);
		await selected(page, 'light');
		await assertPalette(page, 'light');
		await choose(page, 'system');
		await assertPalette(page, 'dark');
		await page.goBack();
		await expect(page).toHaveURL(/\/login$/);
		await selected(page, 'system');
		await assertPalette(page, 'dark');
		expect(await count(), 'listeners must return to the same System-mode baseline').toBe(initialCount);
	}
	expect(await page.evaluate(() => (window as Window & { __appearanceAudit: { documentId: string } }).__appearanceAudit.documentId), 'navigation must retain the document, not hide a leak behind a full reload').toBe(documentId);
	await assertClean(page, errors);
});

test('APP-REG-01: strict CSP and existing PWA metadata remain unchanged after selection and reload', async ({ page }) => {
	const errors = await openAppearance(page, 'light');
	await choose(page, 'dark');
	const response = await page.reload();
	expect(response).not.toBeNull();
	const csp = new Map(response!.headers()['content-security-policy'].split(';').map((directive) => {
		const [name, ...sources] = directive.trim().split(/\s+/);
		return [name, sources];
	}));
	expect(csp.get('script-src')).toContain("'self'");
	expect(csp.get('script-src')).not.toContain("'unsafe-inline'");
	expect(csp.get('script-src')).not.toContain("'unsafe-eval'");
	expect(csp.get('object-src')).toEqual(["'none'"]);
	expect(csp.get('frame-ancestors')).toEqual(["'none'"]);
	await expect(page.locator('link[rel="manifest"]')).toHaveCount(1);
	await expect(page.locator('link[rel="manifest"]')).toHaveAttribute('href', '/manifest.webmanifest');
	await expect(page.locator('meta[name="theme-color"]')).toHaveCount(1);
	await expect(page.locator('meta[name="theme-color"]')).toHaveAttribute('content', '#0b0d12');
	await assertClean(page, errors);
});

test('APP-REG-03: default appearance retains the existing live OS palettes without reloading', async ({ page }, testInfo) => {
	const errors = await openAppearance(page, 'light');
	await assertPalette(page, 'light');
	await testInfo.attach('default-system-light', { body: await page.screenshot(), contentType: 'image/png' });
	await page.emulateMedia({ colorScheme: 'dark' });
	await assertPalette(page, 'dark');
	await testInfo.attach('default-system-dark', { body: await page.screenshot(), contentType: 'image/png' });
	await page.emulateMedia({ colorScheme: 'light' });
	await assertPalette(page, 'light');
	await assertClean(page, errors);
});

test('APP-REG-02: reduced motion remains effective with each explicit palette', async ({ page }) => {
	await page.emulateMedia({ reducedMotion: 'reduce' });
	const errors = await openAppearance(page, 'dark');
	for (const preference of ['light', 'dark', 'system'] as Appearance[]) {
		await choose(page, preference);
		const durations = await selector(page).evaluate((element) => {
			const css = getComputedStyle(element);
			return [css.animationDuration, css.transitionDuration];
		});
		for (const duration of durations) {
			for (const seconds of duration.split(',').map(parseFloat)) expect(seconds).toBeLessThanOrEqual(0.00001);
		}
		expect(await page.evaluate(() => matchMedia('(prefers-reduced-motion: reduce)').matches)).toBe(true);
	}
	await assertClean(page, errors);
});
