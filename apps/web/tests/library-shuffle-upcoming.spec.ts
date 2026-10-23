import { expect, test, type Page } from '@playwright/test';

/**
 * Builder-owned pin: the /library Shuffle upcoming binding.
 *
 * The acceptance shuffle suite drives /demo/player only, and /library is the
 * only production host of the shared PlayerQueue panel — so dropping the
 * `onshuffle` prop (or replacing it with a no-op) stayed green. This spec
 * pins that binding on /library itself: the button is enabled with two
 * upcoming tracks, and one click permutes only the upcoming suffix while the
 * current entry stays put.
 *
 * Harness note (same as `library-player-queue.spec.ts`): /library is
 * client-fetched (`ssr = false`), so the click-driven contract is pinned here
 * in the offline Playwright suite with a route-mocked `/api/library`.
 * Deterministic randomness is injected transiently via addInitScript (never
 * persisted): with `Math.random = () => 0`, a working Fisher–Yates pass over
 * [first, second, third] must yield [first, third, second], which also
 * distinguishes a real shuffle from a no-op handler (an identity result is a
 * valid random outcome, so only the forced draw makes the no-op red).
 */

const ITEMS = [
	{ id: 'row-zulu', title: 'Zulu Song', artist: 'Queue Artist' },
	{ id: 'row-alpha', title: 'Alpha Song', artist: 'Queue Artist' },
	{ id: 'row-mike', title: 'Mike Song', artist: 'Queue Artist' }
];

const [FIRST, SECOND, THIRD] = ITEMS.map((item) => item.id);

async function seedLibrary(page: Page): Promise<void> {
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(ITEMS)
		})
	);
	// Forced draw: transient test-only injection, never persisted as a mode/seed.
	await page.addInitScript(() => {
		Math.random = () => 0;
	});
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
}

function row(page: Page, id: string) {
	return page.locator(`[data-testid="library-item"][data-id="${id}"]`);
}

function queueItem(page: Page, id: string) {
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

test('/library Shuffle upcoming permutes only the upcoming suffix', async ({ page }) => {
	await seedLibrary(page);

	const button = page.getByRole('button', { name: /^shuffle upcoming$/i });
	await expect(button).toHaveCount(1);
	// Empty queue: fewer than two upcoming tracks, so the guard stays disabled.
	await expect(button).toBeDisabled();

	await row(page, FIRST!).click();
	await row(page, SECOND!).click();
	await row(page, THIRD!).click();
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	expect(await queueOrder(page)).toEqual([FIRST, SECOND, THIRD]);
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');

	// Dropping the onshuffle binding disables this button: enabled proves wiring.
	await expect(button).toBeEnabled();
	await button.click();

	// Forced draw must swap the two upcoming tracks; a no-op binding fails here.
	expect(await queueOrder(page)).toEqual([FIRST, THIRD, SECOND]);
	expect((await queueOrder(page)).slice(1).sort()).toEqual([SECOND, THIRD].sort());
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');
	await expect(queueItem(page, FIRST!)).toHaveAttribute('aria-current', 'true');

	const completion = page.getByRole('status').filter({ hasText: /shuffl/i });
	await expect(completion).toHaveCount(1);
	await expect(completion).toContainText(/upcoming|queue/i);
	await expect(completion).not.toContainText(/guaranteed|different order|new order|order changed/i);
});
