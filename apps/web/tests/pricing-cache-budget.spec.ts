import { expect, test, type Page } from '@playwright/test';

/**
 * Pricing page cache budget, retention and priority display.
 *
 * Acceptance contract (display only; no checkout traffic is needed and
 * none may leave the page):
 *
 * - Every pricing row renders readable cache capacity, retention and
 *   priority for its tier, matching the accepted allowance table:
 *   Free 2 GiB / 7 days / low, Plus 20 GiB / 30 days / standard,
 *   Pro 5x 100 GiB / 90 days / high, Pro 20x 400 GiB / 90 days /
 *   highest. Priority stays visible alongside — never instead of —
 *   the budget and retention.
 * - Assistive technology gets the same context: distinct column
 *   headers name the cache budget, the cache retention and the cache
 *   priority, and each plan row keeps its plan as its row header.
 * - The page explains that cached audio is a reclaimable allowance,
 *   not permanent or durable storage, not an offline guarantee and
 *   not provider storage, and it distinguishes the approved/design
 *   allowance from runtime enforcement that is not yet active. It
 *   must not advertise activated cache enforcement.
 * - Existing behavior is preserved: paid-plan button accessible
 *   names and the free-tier link are unchanged.
 */

interface ExpectedRow {
	planName: RegExp;
	capacity: RegExp;
	retention: RegExp;
	priority: RegExp;
}

const EXPECTED_ROWS: ExpectedRow[] = [
	{ planName: /Free trial/, capacity: /\b2\s*GiB\b/, retention: /\b7[-\s]days?\b/i, priority: /\blow\b/i },
	{ planName: /Plus/, capacity: /\b20\s*GiB\b/, retention: /\b30[-\s]days?\b/i, priority: /\bstandard\b/i },
	{ planName: /Pro 5x/, capacity: /\b100\s*GiB\b/, retention: /\b90[-\s]days?\b/i, priority: /\bhigh\b/i },
	{ planName: /Pro 20x/, capacity: /\b400\s*GiB\b/, retention: /\b90[-\s]days?\b/i, priority: /\bhighest\b/i }
];

async function visiblePageText(page: Page): Promise<string> {
	return page.evaluate(() => document.body.innerText.replace(/\s+/g, ' '));
}

test('each pricing row shows its cache capacity, retention and priority', async ({ page }) => {
	await page.goto('/pricing');

	for (const expected of EXPECTED_ROWS) {
		const row = page.getByRole('row', { name: expected.planName });
		await expect(row, `one pricing row for ${expected.planName}`).toHaveCount(1);
		await expect(row, `${expected.planName} row shows readable GiB capacity`).toContainText(
			expected.capacity
		);
		await expect(row, `${expected.planName} row shows retention in days`).toContainText(
			expected.retention
		);
		await expect(
			row,
			`${expected.planName} row still shows its cache priority`
		).toContainText(expected.priority);
	}
});

test('rows do not borrow another tier capacity', async ({ page }) => {
	await page.goto('/pricing');

	// The top-tier capacity appears only on the top tier, and the free
	// tier never carries a paid capacity: values are per-row data, not
	// a shared footnote repeated in every cell.
	await expect(page.getByRole('row', { name: /Free trial/ })).not.toContainText(/\b400\s*GiB\b/);
	await expect(page.getByRole('row', { name: /Free trial/ })).not.toContainText(/\b100\s*GiB\b/);
	await expect(page.getByRole('row', { name: /Pro 20x/ })).not.toContainText(/\b2\s*GiB\b/);
});

test('column headers give budget, retention and priority their own context', async ({ page }) => {
	await page.goto('/pricing');

	await expect(
		page.getByRole('columnheader', { name: /cache[^a-z]*?(budget|capacity|allowance)/i }),
		'a column header names the cache budget/capacity/allowance'
	).toHaveCount(1);
	await expect(
		page.getByRole('columnheader', { name: /retention/i }),
		'a column header names cache retention'
	).toHaveCount(1);
	await expect(
		page.getByRole('columnheader', { name: /priority/i }),
		'a column header names cache priority'
	).toHaveCount(1);
});

test('page explains reclaimable cache without promising durable storage', async ({ page }) => {
	await page.goto('/pricing');
	const text = await visiblePageText(page);

	expect(text, 'page names the cache as reclaimable').toMatch(/reclaimable/i);
	expect(
		text,
		'page says the allowance is not permanent/durable storage'
	).toMatch(/not\s+(permanent|durable)\s+storage/i);
	expect(
		text,
		'page distinguishes the approved/design allowance from enforcement that is not yet active'
	).toMatch(
		/(not yet enforced|enforcement[^.]*\b(planned|not yet|not active|not live)\b|planned allowance|design allowance|approved design)/i
	);

	// Affirmative promises the page must never make.
	expect(text, 'no permanent-storage promise').not.toMatch(/permanent storage included/i);
	expect(text, 'no durable-storage promise').not.toMatch(/durable storage included/i);
	expect(text, 'no storage guarantee').not.toMatch(/guaranteed\s+(offline\s+)?storage/i);
	expect(text, 'no activated-enforcement claim').not.toMatch(
		/cache\s+(limit|budget|cap)s?\s+(are|is)\s+(now\s+)?enforced/i
	);
	expect(text, 'no included provider storage').not.toMatch(/provider storage included/i);
});

test('existing plan actions keep their accessible names (control)', async ({ page }) => {
	await page.goto('/pricing');

	for (const name of ['Choose Plus', 'Choose Pro 5x', 'Choose Pro 20x'] as const) {
		await expect(page.getByRole('button', { name, exact: true })).toHaveCount(1);
	}
	await expect(page.getByRole('link', { name: 'Start trial', exact: true })).toHaveCount(1);
});
