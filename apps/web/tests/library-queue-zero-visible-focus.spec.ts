import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Builder-owned: remove/clear focus recovery when a filter hides survivors.
 *
 * The protected remove-focus bank implements no filter; this file proves the
 * zero-visible vector the card requires: removing the last VISIBLE row while
 * hidden tracks survive must land keyboard focus on the honest no-matches
 * status (`queue-filter-empty`), never the body and never the "Queue is
 * empty" target (which would misannounce a nonempty queue).
 *
 * Harness mirrors the neighbouring library specs: `/library` is
 * client-fetched (`ssr = false`), pinned here with a route-mocked
 * `/api/library`.
 *
 * From apps/web (Node 24 / pnpm 10):
 *   pnpm install --frozen-lockfile
 *   pnpm exec playwright install chromium
 *   CI=1 PUBLIC_API_BASE_URL='' pnpm test:e2e tests/library-queue-zero-visible-focus.spec.ts --workers=1
 */

test.use({ viewport: { width: 1000, height: 700 } });

// Rendered order is API order; ids are deliberately not in sorted order.
const ITEMS = [
	{ id: 'row-zulu', title: 'Zulu Song', artist: 'Queue Artist' },
	{ id: 'row-alpha', title: 'Alpha Song' },
	{ id: 'row-mike', title: 'Mike Song', artist: 'Queue Artist' }
];

async function seedQueue(page: Page): Promise<void> {
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(ITEMS)
		})
	);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await page.locator('[data-testid="library-item"][data-id="row-mike"]').click();
	await page.locator('[data-testid="library-item"][data-id="row-zulu"]').click();
	await page.locator('[data-testid="library-item"][data-id="row-alpha"]').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
}

function queueItem(page: Page, id: string): Locator {
	return page.locator(
		`[data-testid="queue-item"][data-track-id="${id}"], [data-testid="queue-item"][data-id="${id}"]`
	);
}

async function queueOrder(page: Page): Promise<(string | null)[]> {
	return page
		.getByTestId('queue-item')
		.evaluateAll((items) =>
			items.map((item) => item.getAttribute('data-track-id') ?? item.getAttribute('data-id'))
		);
}

test('removing the last visible row focuses the no-matches status, not the body', async ({
	page
}) => {
	await seedQueue(page);

	// Only Zulu Song matches: hidden ids never match named tracks and neither
	// 'Alpha Song' nor 'Mike Song' / 'Queue Artist' contains 'zulu'.
	await page.getByLabel('Filter queue').fill('zulu');
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	await expect(page.getByTestId('queue-filter-count')).toContainText('1 of 3 tracks');

	const remove = queueItem(page, 'row-zulu').getByRole('button', {
		name: 'Remove Zulu Song from queue',
		exact: true
	});
	await remove.focus();
	await remove.press('Enter');

	// Two tracks survive behind the filter: nothing renders, recovery lands on
	// the no-matches status with a visible ring — never the body.
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('queue-filter-count')).toContainText('0 of 2 tracks');
	const status = page.getByTestId('queue-filter-empty');
	await expect(status).toBeFocused();
	await expect(status).toBeVisible();
	await expect(status).toHaveAttribute('tabindex', '-1');
	await expect(status).toHaveText('No matches in this queue');
	const tag = await status.evaluate((el) => el.tagName);
	expect(tag).not.toMatch(/^(BUTTON|A|INPUT|SELECT|TEXTAREA)$/);
	const ring = await status.evaluate((el) => {
		const css = getComputedStyle(el);
		return (
			(css.outlineStyle !== 'none' && parseFloat(css.outlineWidth) > 0) ||
			css.boxShadow !== 'none'
		);
	});
	expect(ring).toBe(true);
	await expect(page.getByTestId('queue-empty-focus')).toHaveCount(0);
	await expect(page.getByTestId('queue-announcement')).toHaveText(
		'Zulu Song removed from the queue.'
	);

	// Clearing the filter proves the hidden tracks survived with order, current
	// and playback untouched: recovery focused a status, nothing more.
	await page.getByRole('button', { name: 'Clear queue filter', exact: true }).click();
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
});
