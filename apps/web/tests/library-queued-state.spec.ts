import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Acceptance: library rows expose their queued state to assistive tech.
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build lands the
 * queued-state exposure on top of this branch and must make these tests
 * pass WITHOUT editing this file.
 *
 * `/library` is client-fetched, so the contract is pinned here in the
 * offline Playwright suite with a route-mocked `/api/library`, like
 * `library-player-queue.spec.ts` (whose queue behavior must not change:
 * selecting a row appends it once, re-selecting is a no-op).
 *
 * A row counts as exposing "queued" when its button reports the state
 * through any standard channel: `aria-pressed="true"`,
 * `aria-selected="true"`, or an accessible name / text that says the
 * track is queued. Queued is membership in the queue, not "currently
 * playing": after Next, the previous track is still queued.
 */

const ITEMS = [
	{ id: 'row-zulu', title: 'Zulu Song', artist: 'Queue Artist' },
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

function rowButton(page: Page, id: string): Locator {
	// Queue control only: each row also carries a star toggle button, so an
	// unfiltered query resolves to 2 elements and trips strict mode.
	return row(page, id).locator('button.nt-library-row');
}

/** True when the row button exposes queued state through any channel. */
async function exposesQueued(button: Locator): Promise<boolean> {
	return button.evaluate((element) => {
		const pressed = element.getAttribute('aria-pressed');
		const selected = element.getAttribute('aria-selected');
		const name = [
			element.getAttribute('aria-label') ?? '',
			element.textContent ?? ''
		].join(' ');
		return pressed === 'true' || selected === 'true' || /\bqueued\b|\bin queue\b/i.test(name);
	});
}

test('selecting a row exposes it as queued and leaves other rows unqueued', async ({ page }) => {
	await seedLibrary(page);

	for (const item of ITEMS) {
		expect(await exposesQueued(rowButton(page, item.id)), `${item.id} pre-click`).toBe(false);
	}

	await row(page, 'row-mike').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');

	await expect
		.poll(() => exposesQueued(rowButton(page, 'row-mike')), { message: 'row-mike queued' })
		.toBe(true);
	expect(await exposesQueued(rowButton(page, 'row-zulu'))).toBe(false);
	expect(await exposesQueued(rowButton(page, 'row-alpha'))).toBe(false);
});

test('re-selecting a queued row keeps one queue entry and stays queued', async ({ page }) => {
	await seedLibrary(page);

	await row(page, 'row-mike').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	await row(page, 'row-mike').click();

	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
	await expect
		.poll(() => exposesQueued(rowButton(page, 'row-mike')), { message: 'row-mike still queued' })
		.toBe(true);
});

test('queued state tracks queue membership, not the now-playing track', async ({ page }) => {
	await seedLibrary(page);

	await row(page, 'row-mike').click();
	await row(page, 'row-zulu').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');

	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');

	// Both selected rows remain queued even though only Zulu is playing.
	await expect
		.poll(() => exposesQueued(rowButton(page, 'row-mike')), { message: 'row-mike queued' })
		.toBe(true);
	await expect
		.poll(() => exposesQueued(rowButton(page, 'row-zulu')), { message: 'row-zulu queued' })
		.toBe(true);
	expect(await exposesQueued(rowButton(page, 'row-alpha'))).toBe(false);
});
