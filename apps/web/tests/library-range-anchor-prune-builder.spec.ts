import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Builder-owned supplement: the range-anchor reset when a refresh snapshot
 * drops the anchor row.
 *
 * The acceptance bank's RNG-B-10 drops the anchor without bringing it back,
 * so the displayed-anchor fallback already degrades the next Shift+click to
 * a plain toggle and the reset itself is never exercised (deleting it keeps
 * the bank green). This spec restores the dropped row with a second refresh
 * before Shift+clicking: without the reset the returned anchor would seed a
 * range, so only the reset keeps the gesture a plain toggle.
 *
 * Pins selection outcome (checked rows, count text) only — never the live
 * region — so it survives either single-announcer reconciliation.
 */

// Rendered order is API order; ids are deliberately not in sorted order.
// Titles avoid "star", "favorite", "select", "append" and "clear" so
// name-filtered locators can only match their intended control.
const ITEMS = [
	{ id: 'row-zulu', title: 'Zulu Song', artist: 'Harbor Lights' },
	{ id: 'row-alpha', title: 'Alpha Song' },
	{ id: 'row-mike', title: 'Mike Song', artist: 'Harbor Lights' },
	{ id: 'row-bravo', title: 'Bravo Song', artist: 'Next Account' },
	{ id: 'row-kilo', title: 'Kilo Song', artist: 'Harbor Lights' },
	{ id: 'row-delta', title: 'Delta Song' }
];

/** Route `/api/library` to a mutable payload so a test can swap it mid-run. */
async function routeLibrary(page: Page, payload: { current: unknown }): Promise<void> {
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(payload.current)
		})
	);
}

async function seedLibrary(page: Page, payload: { current: unknown } = { current: ITEMS }) {
	await routeLibrary(page, payload);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(
		(payload.current as unknown[]).length
	);
}

function row(page: Page, id: string): Locator {
	return page.locator(`[data-testid="library-item"][data-id="${id}"]`);
}

function checkbox(page: Page, id: string): Locator {
	return row(page, id).getByRole('checkbox');
}

/** Checked row IDs in rendered order (hidden rows are unmounted, never listed). */
async function checkedIds(page: Page): Promise<string[]> {
	return page.getByTestId('library-item').evaluateAll((items) =>
		items
			.filter((item) => {
				const box = item.querySelector('input[type="checkbox"]');
				return box instanceof HTMLInputElement && box.checked;
			})
			.map((item) => item.getAttribute('data-id') ?? '')
	);
}

async function refreshLibrary(page: Page, payload: { current: unknown }): Promise<void> {
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(
		(payload.current as unknown[]).length
	);
}

test('a pruned anchor that returns still plain-toggles the next Shift+click', async ({
	page
}) => {
	const payload: { current: unknown } = { current: ITEMS };
	await seedLibrary(page, payload);

	const anchor = checkbox(page, 'row-bravo');
	await expect(anchor, 'checkbox missing on row-bravo').toBeVisible();
	await anchor.click();
	await checkbox(page, 'row-mike').click();
	await expect(page.getByTestId('selection-count')).toHaveText('2 selected');

	// Drop the anchor row (row-mike): prune keeps row-bravo and resets.
	payload.current = ITEMS.filter((item) => item.id !== 'row-mike');
	await refreshLibrary(page, payload);
	await expect(page.getByTestId('selection-count')).toHaveText('1 selected');
	expect(await checkedIds(page)).toEqual(['row-bravo']);

	// Bring the anchor row back: it returns on screen, still unselected.
	payload.current = ITEMS;
	await refreshLibrary(page, payload);
	await expect(page.getByTestId('selection-count')).toHaveText('1 selected');
	await expect(checkbox(page, 'row-mike')).not.toBeChecked();

	// Without the reset, row-mike would seed a mike..kilo range (3 selected).
	await checkbox(page, 'row-kilo').click({ modifiers: ['Shift'] });
	expect(await checkedIds(page)).toEqual(['row-bravo', 'row-kilo']);
	await expect(page.getByTestId('selection-count')).toHaveText('2 selected');
});
