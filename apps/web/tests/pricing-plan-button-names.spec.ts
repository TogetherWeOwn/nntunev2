import { expect, test, type Page } from '@playwright/test';

/**
 * Pricing plan button accessible names.
 *
 * Each plan action must expose a distinct accessible name that names its
 * plan, so screen-reader and voice-control users can tell the paid tiers
 * apart. The free tier stays a link ("Start trial"); every paid tier is a
 * button whose name includes the plan ("Choose Plus", …) and whose pending
 * state keeps the plan ("Starting Plus…").
 *
 * All checkout traffic is intercepted and fulfilled locally; no live
 * checkout request ever leaves the page.
 */

const EXPECTED_PAID_NAMES = ['Choose Plus', 'Choose Pro 5x', 'Choose Pro 20x'] as const;

/** Rendered accessible name per paid-plan button, in table order. */
async function paidButtonAccessibleNames(page: Page): Promise<string[]> {
	return page.evaluate(() => {
		const buttons = [...document.querySelectorAll('table tbody tr td button')];
		return buttons.map(
			(button) => button.getAttribute('aria-label') ?? button.textContent?.trim() ?? ''
		);
	});
}

test('each paid plan button exposes a distinct accessible name', async ({ page }, testInfo) => {
	await page.goto('/pricing');

	const names = await paidButtonAccessibleNames(page);
	testInfo.annotations.push({
		type: 'accessible-names',
		description: JSON.stringify(names)
	});
	expect(names, 'pricing table must render three paid-plan buttons').toHaveLength(3);
	expect(
		new Set(names).size,
		`paid-plan button names must be distinct, got ${JSON.stringify(names)}`
	).toBe(names.length);
	expect(names).toEqual([...EXPECTED_PAID_NAMES]);

	// Role-tree level: each name resolves to exactly one button, and no
	// generic "Choose" button remains.
	for (const name of EXPECTED_PAID_NAMES) {
		const button = page.getByRole('button', { name, exact: true });
		await expect(button, `exactly one button named "${name}"`).toHaveCount(1);
		await expect(button).toBeVisible();
	}
	await expect(page.getByRole('button', { name: 'Choose', exact: true })).toHaveCount(0);

	// Free tier stays a single distinct link.
	await expect(page.getByRole('link', { name: 'Start trial', exact: true })).toHaveCount(1);
});

test('pending plan button keeps a distinct accessible name', async ({ page }, testInfo) => {
	let openGate!: () => void;
	const gate = new Promise<void>((resolve) => {
		openGate = resolve;
	});
	await page.route('**/v1/billing/checkout', async (route) => {
		await gate;
		await route.fulfill({
			status: 500,
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify({ code: 'internal_error', message: 'checkout unavailable (fixture)' })
		});
	});

	await page.goto('/pricing');
	// Locate by row so the click works regardless of the button's name.
	await page.getByRole('row', { name: /Plus/ }).getByRole('button').click();

	const pendingNames = await paidButtonAccessibleNames(page);
	testInfo.annotations.push({
		type: 'accessible-names-pending',
		description: JSON.stringify(pendingNames)
	});
	expect(new Set(pendingNames).size, 'pending names must stay distinct').toBe(
		pendingNames.length
	);
	await expect(page.getByRole('button', { name: 'Starting Plus…', exact: true })).toHaveCount(1);
	await expect(page.getByRole('button', { name: 'Choose Pro 5x', exact: true })).toHaveCount(1);
	await expect(page.getByRole('button', { name: 'Choose Pro 20x', exact: true })).toHaveCount(1);
	await expect(page.getByRole('button', { name: 'Starting…', exact: true })).toHaveCount(0);

	openGate();
	await expect(page.getByRole('alert')).toContainText('Start checkout failed (500)');
	const restored = await paidButtonAccessibleNames(page);
	expect(restored).toEqual([...EXPECTED_PAID_NAMES]);
});
