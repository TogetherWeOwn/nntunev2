import { expect, test, type Locator, type Page } from '@playwright/test';

// Builder-owned regression: complements the acceptance-owned
// library-browse-announcements.spec.ts (left untouched) by pinning two
// shared-node behaviours that bank does not exercise: a repeated identical
// outcome is re-announced (versioned re-announce), and reselecting the
// current view stays silent. Safe for the Builder to extend.
//
// Focused command (repo root, deterministic, offline):
//   CI=1 PUBLIC_API_BASE_URL='' pnpm --dir apps/web exec playwright test \
//     tests/library-browse-announcements-regression.spec.ts --workers=1 --retries=0

const ITEMS = [
	{ id: 'repeat-alpha', title: 'Alpha Song', artist: 'Harbor Lights' },
	{ id: 'repeat-gamma', title: 'Gamma Tune', artist: 'Echo Field' }
];

async function openLibrary(page: Page): Promise<Locator> {
	await page.route('**/api/library', (route) =>
		route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(ITEMS) })
	);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	return page.getByTestId('library-browse-status');
}

/** DOM mutations on the shared browse node while an action runs. */
async function mutationsDuring(
	page: Page,
	node: Locator,
	action: () => Promise<unknown>
): Promise<number> {
	await node.evaluate((element) => {
		const win = element.ownerDocument.defaultView as Window & {
			__brMutations?: number;
			__brObserver?: MutationObserver;
		};
		win.__brObserver?.disconnect();
		win.__brMutations = 0;
		win.__brObserver = new MutationObserver((records) => {
			win.__brMutations = (win.__brMutations ?? 0) + records.length;
		});
		win.__brObserver.observe(element, {
			childList: true,
			characterData: true,
			subtree: true,
			attributes: true
		});
	});
	await action();
	// Mutation records dispatch on the microtask queue; a short settle window
	// keeps the zero assertion honest without polling for absence.
	await page.waitForTimeout(200);
	return node.evaluate((element) => {
		const win = element.ownerDocument.defaultView as Window & {
			__brMutations?: number;
			__brObserver?: MutationObserver;
		};
		win.__brObserver?.disconnect();
		win.__brObserver = undefined;
		return win.__brMutations ?? 0;
	});
}

test('[BA-R01] a repeated identical outcome is re-announced through the shared node', async ({
	page
}) => {
	const node = await openLibrary(page);
	const select = page.getByRole('checkbox', { name: 'Select Alpha Song', exact: true });
	const clearSelection = page.getByRole('button', { name: 'Clear selection', exact: true });
	await select.check();
	await clearSelection.click();
	await expect(node).toHaveText('Selection cleared');
	await select.check();
	await expect(page.getByTestId('selection-count')).toHaveText('1 selected');
	const repeatedSelection = await mutationsDuring(page, node, () => clearSelection.click());
	expect(repeatedSelection, 'a second "Selection cleared" must re-render the live node').toBeGreaterThan(0);
	await expect(node).toHaveText('Selection cleared');

	const field = page.getByRole('searchbox', { name: /find|search/i });
	const clearSearch = page.getByRole('button', { name: 'Clear search', exact: true }).first();
	await field.fill('tune');
	await clearSearch.click();
	await expect(node).toHaveText('Search cleared');
	await field.fill('tune');
	await expect(page.getByTestId('library-item')).toHaveCount(1);
	const repeatedSearch = await mutationsDuring(page, node, () => clearSearch.click());
	expect(repeatedSearch, 'a second "Search cleared" must re-render the live node').toBeGreaterThan(0);
	await expect(node).toHaveText('Search cleared');
});

test('[BA-R02] reselecting the current view leaves the shared node untouched', async ({ page }) => {
	const node = await openLibrary(page);
	const view = page.getByLabel('Show', { exact: true });
	await expect(view.locator('option')).toHaveCount(3);
	await view.selectOption({ label: 'Favorites only' });
	await expect(node).toHaveText('Showing Favorites only');
	const reselected = await mutationsDuring(page, node, () =>
		view.selectOption({ label: 'Favorites only' })
	);
	expect(reselected, 'reselecting the current view must not re-announce').toBe(0);
	await expect(node).toHaveText('Showing Favorites only');
});
