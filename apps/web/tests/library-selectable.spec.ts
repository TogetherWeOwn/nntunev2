import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Acceptance: library rows are selectable (M1-f, queue-order un-fixme step 1).
 *
 * OWNERSHIP: this file is acceptance-owned. The build card must make these
 * tests pass WITHOUT editing this file; a wrong test comes back to the
 * acceptance card instead.
 *
 * Harness note: this page's interactive surface is client-fetched
 * (`ssr = false`, component fetches `/api/library` on mount) and the vitest
 * setup in `vite.config.ts` is SSR-render/source-pin only — there is no DOM
 * component-test environment in this package. The offline Playwright library
 * specs (`library-cache.spec.ts`) are the repo's interactive harness for this
 * page, so the click/keyboard component contract is pinned here with a
 * route-mocked `/api/library`.
 *
 * Contract under test (from the paired build card's acceptance criteria):
 * - Each library row (`library-item`, `data-id` = track id) is operable by
 *   mouse click on the row itself — the staging queue-order journey clicks
 *   rows (`e2e/tests/player-queue-order.spec.ts`), so whole-row clickability
 *   is part of the downstream contract, not just a small inner button.
 * - Each row is keyboard-operable: its control (the row itself as
 *   `role=option`/button semantics, or a descendant button) is focusable and
 *   both Enter and Space select it while focused (native button behavior).
 * - Selecting rows surfaces them, in selection-gesture order, as
 *   `queue-item` elements carrying the track id (`data-track-id`, falling
 *   back to `data-id` — the same read as the staging queue-order spec).
 *   Selection order, not rendered/sorted order: the fixtures below are
 *   deliberately unordered by id so a sorted or library-order readout fails.
 *
 * Out of scope (not pinned): queue advancement/player behavior, toggle-off
 * semantics, duplicate selection.
 */

// Rendered order is API order; ids are deliberately not in sorted order.
const ITEMS = [
	{ id: 'row-zulu', title: 'Zulu Song', artist: 'Select Artist' },
	{ id: 'row-alpha', title: 'Alpha Song' },
	{ id: 'row-mike', title: 'Mike Song' }
];

async function seedLibrary(page: Page): Promise<void> {
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(ITEMS)
		})
	);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
}

function row(page: Page, id: string): Locator {
	return page.locator(`[data-testid="library-item"][data-id="${id}"]`);
}

/**
 * The row's keyboard-operable control: a descendant button/option if the
 * build puts the affordance inside the row, else the row itself.
 */
async function rowControl(page: Page, id: string): Promise<Locator> {
	const inner = row(page, id).locator('button, [role="button"], [role="option"]').first();
	return (await inner.count()) > 0 ? inner : row(page, id);
}

async function selectedOrder(page: Page): Promise<(string | null)[]> {
	return page
		.getByTestId('queue-item')
		.evaluateAll((items) =>
			items.map((item) => item.getAttribute('data-track-id') ?? item.getAttribute('data-id'))
		);
}

test('library rows render in API order with track-id hooks', async ({ page }) => {
	await seedLibrary(page);

	const rendered = await page
		.getByTestId('library-item')
		.evaluateAll((items) => items.map((item) => item.getAttribute('data-id')));
	expect(rendered).toEqual(['row-zulu', 'row-alpha', 'row-mike']);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
});

test('clicking rows selects them in click order', async ({ page }) => {
	await seedLibrary(page);

	// Click against rendered and sorted order: 3rd row, then 1st.
	await row(page, 'row-mike').click();
	await row(page, 'row-zulu').click();

	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	expect(await selectedOrder(page)).toEqual(['row-mike', 'row-zulu']);
});

test('Enter on a focused row selects it', async ({ page }) => {
	await seedLibrary(page);

	const control = await rowControl(page, 'row-alpha');
	await control.focus();
	await expect(control, 'row control must be keyboard-focusable').toBeFocused();
	await page.keyboard.press('Enter');

	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	expect(await selectedOrder(page)).toEqual(['row-alpha']);
});

test('Space on a focused row selects it', async ({ page }) => {
	await seedLibrary(page);

	const control = await rowControl(page, 'row-zulu');
	await control.focus();
	await expect(control, 'row control must be keyboard-focusable').toBeFocused();
	await page.keyboard.press('Space');

	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	expect(await selectedOrder(page)).toEqual(['row-zulu']);
});

test('mixed click and keyboard selection preserves gesture order', async ({ page }) => {
	await seedLibrary(page);

	await row(page, 'row-mike').click();
	const control = await rowControl(page, 'row-zulu');
	await control.focus();
	await expect(control, 'row control must be keyboard-focusable').toBeFocused();
	await page.keyboard.press('Enter');
	await row(page, 'row-alpha').click();

	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	expect(await selectedOrder(page)).toEqual(['row-mike', 'row-zulu', 'row-alpha']);
});
