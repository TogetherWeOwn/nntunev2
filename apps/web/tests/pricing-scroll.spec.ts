import { expect, test, type Page } from '@playwright/test';

const viewportWidth = 390;

async function openPricing(page: Page) {
	await page.route('**/v1/plans', (route) =>
		route.fulfill({ status: 401, json: { code: 'unauthorized' } })
	);
	await page.goto('/pricing');
}

test('the pricing table scrolls locally and keeps its keyboard actions', async ({ page }) => {
	await openPricing(page);
	const region = page.getByRole('region', { name: /^Monthly plans/ });
	const table = region.getByRole('table', { name: /^Monthly plans/ });
	const choosePlus = table.getByRole('button', { name: 'Choose Plus', exact: true });
	await expect(choosePlus).toBeEnabled();
	await expect(table.getByRole('columnheader')).toHaveCount(5);
	await expect(table.getByRole('rowheader')).toHaveCount(3);
	for (const width of [1280, viewportWidth, 320]) {
		await page.setViewportSize({ width, height: 800 });
		const geometry = await region.evaluate((el) => ({
			documentWidth: document.documentElement.scrollWidth,
			visibleWidth: document.documentElement.clientWidth,
			overflow: el.scrollWidth - el.clientWidth
		}));
		expect(geometry.documentWidth).toBe(geometry.visibleWidth);
		if (width === 1280) expect(geometry.overflow).toBe(0);
		else expect(geometry.overflow).toBeGreaterThan(0);
	}

	await region.focus();
	await page.keyboard.press('ArrowRight');
	await expect.poll(() => region.evaluate((el) => el.scrollLeft)).toBeGreaterThan(0);
	await page.keyboard.press('Tab');
	await expect(choosePlus).toBeFocused();
	await page.keyboard.press('Tab');
	await expect(table.getByRole('button', { name: 'Choose Pro', exact: true })).toBeFocused();
	await page.keyboard.press('Tab');
	await expect(table.getByRole('button', { name: 'Choose Max', exact: true })).toBeFocused();
	expect(await page.evaluate(() => window.scrollX)).toBe(0);
});

async function openPaywall(page: Page) {
	await page.route('**/v1/billing/checkout', (route) =>
		route.fulfill({ status: 402, json: { code: 'plan_limit_exceeded' } })
	);
	await openPricing(page);
	const opener = page.getByRole('button', { name: 'Choose Plus', exact: true });
	await expect(opener).toBeEnabled();
	await opener.press('Enter');
	const dialog = page.getByRole('dialog');
	await expect(dialog).toBeVisible();
	await dialog.evaluate((el) => { el.scrollTop = 0; });
	await page.evaluate(() => window.scrollTo(0, 75));
	await expect.poll(() => page.evaluate(() => window.scrollY)).toBe(75);
	return { dialog, opener };
}

// Give the browser time to commit compositor scrolling after trusted input.
async function scrollPosition(page: Page) {
	return page.evaluate(async () => {
		await new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
		return window.scrollY;
	});
}

for (const height of [490, 160]) {
	test(`paywall keeps wheel scrolling off the page at ${height}px height`, async ({ page }) => {
		await page.setViewportSize({ width: viewportWidth, height });
		const { dialog, opener } = await openPaywall(page);
		const bounds = (await dialog.boundingBox())!;
		const x = bounds.x + bounds.width / 2;
		const y = bounds.y + bounds.height / 2;
		expect(bounds.x).toBeGreaterThan(4);

		await page.mouse.move(4, y);
		await page.mouse.wheel(0, 450);
		expect(await scrollPosition(page)).toBe(75);

		const content = await dialog.evaluate((el) => ({ max: el.scrollHeight - el.clientHeight, top: el.scrollTop }));
		expect(content.top).toBe(0);
		if (height === 160) expect(content.max).toBeGreaterThan(0);
		else expect(content.max).toBe(0);
		await page.mouse.move(x, y);
		await page.mouse.wheel(0, 450);
		await expect.poll(() => dialog.evaluate((el) => el.scrollTop)).toBe(content.max);
		expect(await scrollPosition(page)).toBe(75);
		await page.mouse.wheel(0, 450);
		expect(await scrollPosition(page)).toBe(75);

		await page.keyboard.press('Escape');
		await expect(dialog).toBeHidden();
		await expect(opener).toBeFocused();
	});
}

test.describe('coarse-pointer paywall', () => {
	test.use({ viewport: { width: viewportWidth, height: 490 }, isMobile: true, hasTouch: true });

	test('paywall contains touch pans and the backdrop still dismisses', async ({ page, context }) => {
		const { dialog, opener } = await openPaywall(page);
		expect(await page.evaluate(() => matchMedia('(pointer: coarse)').matches)).toBe(true);
		const bounds = (await dialog.boundingBox())!;
		const y = bounds.y + bounds.height - 12;
		expect(bounds.x).toBeGreaterThan(4);
		const input = await context.newCDPSession(page);

		for (const x of [4, bounds.x + bounds.width / 2]) {
			await input.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x, y }] });
			for (const distance of [20, 40, 60, 80]) {
				await input.send('Input.dispatchTouchEvent', {
					type: 'touchMove', touchPoints: [{ x, y: y - distance }]
				});
			}
			await input.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
			expect(await scrollPosition(page)).toBe(75);
		}

		await page.touchscreen.tap(4, y);
		await expect(dialog).toBeHidden();
		await expect(opener).toBeFocused();
	});
});
