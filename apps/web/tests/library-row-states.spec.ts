import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Acceptance: visible library row states and track count.
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build lands the
 * badges, honest already-queued state and count summary on top of this
 * branch and must make these tests pass WITHOUT editing this file; a wrong
 * test comes back to the acceptance card instead.
 *
 * Harness note (same as `library-selectable.spec.ts`): `/library` is
 * client-fetched (`ssr = false`, component fetches `/api/library` on mount)
 * and the vitest setup in `vite.config.ts` is SSR-render/source-pin only,
 * so the interactive contract is pinned here in the offline Playwright
 * suite with a route-mocked `/api/library`. No test in this file needs the
 * network: every `/api/library` response is fulfilled from fixtures.
 *
 * Stable case IDs:
 * - LIBROWS-01: queueing a row shows a visible Queued badge on that row only.
 * - LIBROWS-02: badge presence agrees with the queued aria-label on every
 *   row (same source, never diverging), surviving player-next.
 * - LIBROWS-03: an already-queued row reports aria-pressed="true";
 *   unqueued rows do not; queued rows stay enabled.
 * - LIBROWS-04: re-activating a queued row neither duplicates, errors nor
 *   steals focus.
 * - LIBROWS-05: a three-track library heads the list with "3 tracks"
 *   (precedes the list, outside live regions, stable while queueing).
 * - LIBROWS-06: a one-track library reads "1 track".
 * - LIBROWS-07: an empty library keeps the empty-state message and renders
 *   no count (guard; green before the build).
 * - LIBROWS-08: refresh reconciles badges, pressed state and count with the
 *   new list.
 * - LIBROWS-09: narrow screens keep the badge, count and row operability
 *   on-screen.
 * - LIBROWS-10: the queued badge renders visibly in dark mode.
 *
 * Contract under test (paired build card):
 * - A queued row shows a visible badge (`library-queued-badge` inside the
 *   `library-item` row) whose text says Queued. The badge derives from the
 *   same queue-membership source as the "— Queued" aria-label suffix:
 *   LIBROWS-02 sweeps every row and requires badge-visible to agree with
 *   label-says-queued in each observed state (after queueing, after Next,
 *   after refresh). DOM agreement is the observable form of "same source".
 * - Already-queued rows use the pressed choice (not disabled): the row
 *   button reports `aria-pressed="true"`, stays enabled and focusable, and
 *   re-activation is an idempotent no-op. Disabled would drop queued rows
 *   from the tab order and change the keyboard contract pinned by
 *   `library-selectable.spec.ts`; pressed keeps it intact. Unqueued rows
 *   must not report pressed-true (absent or "false" both satisfy this).
 * - A `library-track-count` summary heads the list with "N tracks" /
 *   "1 track" sourced from the loaded items (queueing does not change it),
 *   placed before the list in document order and outside any live region
 *   that would over-announce. An empty library renders no count: the
 *   existing "Your library is empty" status message stays the honest empty
 *   indicator (LIBROWS-07).
 * - Each row keeps exactly one button (the existing row control): the
 *   badge is status text, not a second control.
 *
 * Hooks are literals, not TESTIDS entries: the route-contract unit test
 * maps every TESTIDS key to a single route source, and — like the queue
 * hooks (`queue-item`, `now-playing`, `player-next`) pinned by
 * `library-player-queue.spec.ts` — these row hooks live below the route
 * contract. Adding TESTIDS entries would fail that unit test on the
 * pre-build code, so the suite pins the literal `data-testid` values here.
 *
 * No vitest units accompany this suite on purpose: badge derivation,
 * pressed mapping and pluralization are component-local (the library page
 * derives them at render), and no importable helper exists on the
 * pre-build code. Inventing import paths would make RED a compile failure
 * instead of an assertion failure, so every behavior above is pinned
 * through the DOM instead.
 *
 * Out of scope (not pinned): queue editing/reordering/filtering/sorting,
 * PlayerQueue changes, durations/artwork, playback, deployment.
 */

// Rendered order is API order; ids are deliberately not in sorted order.
const ITEMS = [
	{ id: 'row-zulu', title: 'Zulu Song', artist: 'Row Artist' },
	{ id: 'row-alpha', title: 'Alpha Song' },
	{ id: 'row-mike', title: 'Mike Song' }
];

async function seedLibrary(page: Page, items: unknown[] = ITEMS): Promise<void> {
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(items)
		})
	);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(items.length);
}

function row(page: Page, id: string): Locator {
	return page.locator(`[data-testid="library-item"][data-id="${id}"]`);
}

function rowButton(page: Page, id: string): Locator {
	return row(page, id).getByRole('button');
}

function badgeFor(page: Page, id: string): Locator {
	return row(page, id).getByTestId('library-queued-badge');
}

async function isPressedTrue(button: Locator): Promise<boolean> {
	return (await button.getAttribute('aria-pressed')) === 'true';
}

async function labelSaysQueued(button: Locator): Promise<boolean> {
	return /\bqueued\b/i.test((await button.getAttribute('aria-label')) ?? '');
}

async function expectBadgesMatchLabels(page: Page, ids: string[]): Promise<void> {
	for (const id of ids) {
		const saysQueued = await labelSaysQueued(rowButton(page, id));
		await expect(
			badgeFor(page, id),
			`${id}: badge presence must agree with the queued aria-label`
		).toHaveCount(saysQueued ? 1 : 0);
	}
}

async function queueOrder(page: Page): Promise<(string | null)[]> {
	return page
		.getByTestId('queue-item')
		.evaluateAll((items) =>
			items.map((item) => item.getAttribute('data-track-id') ?? item.getAttribute('data-id'))
		);
}

test('LIBROWS-01: queueing a row shows a visible Queued badge on that row only', async ({
	page
}) => {
	await seedLibrary(page);

	await expect(page.getByTestId('library-queued-badge')).toHaveCount(0);

	await row(page, 'row-mike').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);

	const badge = badgeFor(page, 'row-mike');
	await expect(badge).toBeVisible();
	await expect(badge).toContainText(/queued/i);
	await expect(badgeFor(page, 'row-zulu')).toHaveCount(0);
	await expect(badgeFor(page, 'row-alpha')).toHaveCount(0);
});

test('LIBROWS-02: badge presence agrees with the queued aria-label on every row', async ({
	page
}) => {
	await seedLibrary(page);

	await row(page, 'row-zulu').click();
	await row(page, 'row-alpha').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	await expectBadgesMatchLabels(page, ['row-zulu', 'row-alpha', 'row-mike']);

	// Queued is membership, not now-playing: advancing must not diverge them.
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Alpha Song');
	await expectBadgesMatchLabels(page, ['row-zulu', 'row-alpha', 'row-mike']);
});

test('LIBROWS-03: an already-queued row reports aria-pressed=true, unqueued rows do not', async ({
	page
}) => {
	await seedLibrary(page);

	await row(page, 'row-mike').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);

	await expect
		.poll(() => isPressedTrue(rowButton(page, 'row-mike')), { message: 'row-mike pressed' })
		.toBe(true);
	expect(await isPressedTrue(rowButton(page, 'row-zulu'))).toBe(false);
	expect(await isPressedTrue(rowButton(page, 'row-alpha'))).toBe(false);
	// The honest state is pressed, not disabled: queued rows stay operable.
	await expect(rowButton(page, 'row-mike')).toBeEnabled();
});

test('LIBROWS-04: re-activating a queued row neither duplicates, errors nor steals focus', async ({
	page
}) => {
	const pageErrors: string[] = [];
	page.on('pageerror', (error) => pageErrors.push(String(error)));

	await seedLibrary(page);

	const control = rowButton(page, 'row-mike');
	await control.focus();
	await expect(control).toBeFocused();
	await page.keyboard.press('Enter');
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	await expect(control).toBeFocused();

	// Keyboard re-activation of the queued row.
	await page.keyboard.press('Enter');
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	expect(await queueOrder(page)).toEqual(['row-mike']);
	await expect(control, 'focus must stay on the re-activated row').toBeFocused();

	// Mouse re-click of the queued row.
	await row(page, 'row-mike').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	expect(await queueOrder(page)).toEqual(['row-mike']);
	await expect(control, 'focus must stay on the re-clicked row').toBeFocused();

	await expect(page.getByRole('alert')).toHaveCount(0);
	expect(pageErrors, `page errors: ${pageErrors.join('; ')}`).toEqual([]);
	await expect
		.poll(() => isPressedTrue(control), { message: 'row-mike still pressed' })
		.toBe(true);
});

test('LIBROWS-05: a three-track library heads the list with "3 tracks"', async ({ page }) => {
	await seedLibrary(page);

	const count = page.getByTestId('library-track-count');
	await expect(count).toBeVisible();
	await expect(count).toHaveText('3 tracks');

	// "Heads the list": the summary precedes the list in document order.
	const order = await page.evaluate(() => {
		const summary = document.querySelector('[data-testid="library-track-count"]');
		const list = document.querySelector('[data-testid="library-list"]');
		if (!summary || !list) return null;
		return summary.compareDocumentPosition(list) & Node.DOCUMENT_POSITION_FOLLOWING
			? 'count-first'
			: 'list-first';
	});
	expect(order).toBe('count-first');

	// Outside over-announcing live regions: neither the count nor an
	// ancestor may carry a live-region role or aria-live.
	const liveAncestor = await count.evaluate((element) => {
		let current: HTMLElement | null = element as HTMLElement;
		while (current) {
			const role = current.getAttribute('role');
			if (
				current.hasAttribute('aria-live') ||
				(role !== null && ['status', 'alert', 'log'].includes(role))
			) {
				return current.tagName;
			}
			current = current.parentElement;
		}
		return null;
	});
	expect(liveAncestor).toBeNull();

	// The count follows the loaded items, not the queue.
	await row(page, 'row-mike').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	await expect(count).toHaveText('3 tracks');
});

test('LIBROWS-06: a one-track library reads "1 track"', async ({ page }) => {
	await seedLibrary(page, [{ id: 'row-solo', title: 'Solo Song', artist: 'Only Artist' }]);

	const count = page.getByTestId('library-track-count');
	await expect(count).toBeVisible();
	await expect(count).toHaveText('1 track');
});

test('LIBROWS-07: an empty library keeps the empty-state message and renders no count', async ({
	page
}) => {
	await seedLibrary(page, []);

	await expect(page.getByRole('status')).toContainText(/library is empty/i);
	await expect(page.getByTestId('library-track-count')).toHaveCount(0);
	await expect(page.getByTestId('library-list')).toHaveCount(0);
});

test('LIBROWS-08: refresh reconciles badges, pressed state and count with the new list', async ({
	page
}) => {
	const payload: { current: unknown } = { current: ITEMS };
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(payload.current)
		})
	);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(page.getByTestId('library-track-count')).toHaveText('3 tracks');

	await row(page, 'row-mike').click();
	await row(page, 'row-zulu').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	await expect(badgeFor(page, 'row-mike')).toBeVisible();
	await expect(badgeFor(page, 'row-zulu')).toBeVisible();

	// Refresh drops row-mike, keeps row-zulu, adds row-novel.
	payload.current = [
		{ id: 'row-zulu', title: 'Zulu Song', artist: 'Row Artist' },
		{ id: 'row-novel', title: 'Novel Song' }
	];
	await page.getByRole('button', { name: 'Refresh' }).click();

	// Count follows the new list; the queue drops the vanished track.
	await expect(page.getByTestId('library-track-count')).toHaveText('2 tracks');
	await expect(page.getByTestId('library-item')).toHaveCount(2);
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');

	// Survivor keeps badge + pressed; newcomer has neither.
	await expect(badgeFor(page, 'row-zulu')).toBeVisible();
	await expect(badgeFor(page, 'row-novel')).toHaveCount(0);
	await expect
		.poll(() => isPressedTrue(rowButton(page, 'row-zulu')), { message: 'row-zulu pressed' })
		.toBe(true);
	expect(await isPressedTrue(rowButton(page, 'row-novel'))).toBe(false);
	await expectBadgesMatchLabels(page, ['row-zulu', 'row-novel']);
});

test('LIBROWS-09: narrow screens keep the badge, count and row operability on-screen', async ({
	page
}) => {
	await page.setViewportSize({ width: 360, height: 800 });
	await seedLibrary(page);

	await expect(page.getByTestId('library-track-count')).toBeVisible();

	await row(page, 'row-mike').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);

	const badge = badgeFor(page, 'row-mike');
	await expect(badge).toBeVisible();
	await expect(badge).toContainText(/queued/i);

	// The badge stays inside the viewport instead of pushing content off-screen.
	const box = await badge.boundingBox();
	expect(box, 'badge must have a layout box').not.toBeNull();
	expect(box?.x ?? -1).toBeGreaterThanOrEqual(0);
	expect((box?.x ?? 361) + (box?.width ?? 361)).toBeLessThanOrEqual(360);

	// Rows stay operable at this width: queueing a second row works.
	await row(page, 'row-zulu').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
});

test.describe('dark color scheme', () => {
	test.use({ colorScheme: 'dark' });

	test('LIBROWS-10: the queued badge renders visibly in dark mode', async ({ page }) => {
		await seedLibrary(page);

		await row(page, 'row-mike').click();
		await expect(page.getByTestId('queue-item')).toHaveCount(1);

		const badge = badgeFor(page, 'row-mike');
		await expect(badge).toBeVisible();
		await expect(badge).toContainText(/queued/i);
	});
});
