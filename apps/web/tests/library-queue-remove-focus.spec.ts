import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Acceptance: keyboard focus survives queue removal and clear on /library.
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build lands the
 * remove/clear focus recovery on top of this branch and must make these
 * tests pass WITHOUT editing this file; a wrong test comes back to the
 * acceptance card instead.
 *
 * Harness note (same as the neighbouring library specs): `/library` is
 * client-fetched (`ssr = false`), so the keyboard contract is pinned here in
 * the offline Playwright suite with a route-mocked `/api/library`. Every
 * test guards its first control interaction with an explicit visibility
 * assertion, so a run against a build without the controls fails as an
 * assertion (missing control), never as a click error. RED here means the
 * recovery behaviour is missing: removals already work (order/current
 * assertions pass) while focus falls to the document body.
 *
 * Hooks: the established queue literals `queue-item` (carrying
 * `data-track-id`), `now-playing`, `player-next` and the polite
 * `queue-announcement` region, plus the new component-owned literal
 * `queue-empty-focus` pinned by this bank (like `queue-item`, it lives in
 * the component, so it is never a route TESTIDS entry): a non-activatable
 * heading/status target with `tabindex="-1"` that receives focus when no
 * rows survive. Per-row `Remove {title} from queue` controls and the
 * `Clear queue` button keep their native button semantics.
 *
 * Contract under test (paired build card):
 * - When the activated Remove held focus, focus moves to the next surviving
 *   displayed row's Remove control, otherwise the previous; when no rows
 *   survive (sole removal, Clear), focus moves to `queue-empty-focus` —
 *   never the now-disabled Clear button, never the body.
 * - Consecutive keyboard removals stay operable: each Enter removes exactly
 *   one row and focus stays inside the queue until it empties.
 * - Recovery fires only when the explicit action would strand focus and no
 *   newer focus movement, pointer flow, background refresh or auth purge has
 *   superseded it. Survivors are re-resolved by stable track id.
 * - Each remove/clear announces a concise outcome exactly once through
 *   `queue-announcement`; previous move/reveal messages are replaced, never
 *   replayed. Selection, order, reveal, radio/repeat and transport intent
 *   are unchanged: recovery never plays, seeks or repopulates.
 *
 * Queue filtering has NOT landed at the RED base (no filter control in the
 * library route or the queue component): cases use the surviving displayed
 * rows directly. If a filter lands underneath, "next/previous surviving
 * displayed row" means visible rows and the empty target covers the
 * zero-visible case — this file implements no filter.
 *
 * Out of scope (not pinned): undo, new queue actions, selection policy,
 * favorites/recent-history focus, app-shell navigation, audio transport and
 * the background-refresh/auth details owned by the sibling retention and
 * announcement suites. Theme cases emulate the OS scheme only; palette
 * rendering stays with the appearance suite.
 *
 * From apps/web (Node 24 / pnpm 10):
 *   pnpm install --frozen-lockfile
 *   pnpm exec playwright install chromium
 *   CI=1 PUBLIC_API_BASE_URL='' pnpm test:e2e tests/library-queue-remove-focus.spec.ts --workers=1
 * Regression: same command with tests/library-queue-manage.spec.ts and
 *   tests/library-queue-orientation.spec.ts; plus the SSR companion
 *   (pnpm test src/lib/player/playerQueue.removeFocus.test.ts) and pnpm check.
 * Ensure preview port 4173 is free; CI=1 forces this run's own server, never
 * another worktree's preview.
 * Expected pre-feature RED: QF-B01..B09 and QF-B15..B18 fail on focus or
 * announcement assertions (activeElement is body, region stays empty); the
 * QF-B00/B10..B14/B19 controls pass. Builder next action: implement recovery
 * in the shared queue component plus host remove/clear announcements, then
 * run this file plus the protected regressions and same-head PR gates.
 */

test.use({ viewport: { width: 1000, height: 700 } });

// Rendered order is API order; ids are deliberately not in sorted order.
const ITEMS = [
	{ id: 'row-zulu', title: 'Zulu Song', artist: 'Queue Artist' },
	{ id: 'row-alpha', title: 'Alpha Song' },
	{ id: 'row-mike', title: 'Mike Song', artist: 'Queue Artist' }
];

async function seedLibrary(
	page: Page,
	payload: { current: unknown } = { current: ITEMS }
): Promise<void> {
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(payload.current)
		})
	);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
}

function row(page: Page, id: string): Locator {
	return page.locator(`[data-testid="library-item"][data-id="${id}"]`);
}

function queueItem(page: Page, id: string): Locator {
	return page.locator(
		`[data-testid="queue-item"][data-track-id="${id}"], [data-testid="queue-item"][data-id="${id}"]`
	);
}

/** A management button by its accessible name, scoped to one queue row. */
function rowButton(page: Page, trackId: string, name: string): Locator {
	return queueItem(page, trackId).getByRole('button', { name, exact: true });
}

function emptyTarget(page: Page): Locator {
	return page.getByTestId('queue-empty-focus');
}

function announcement(page: Page): Locator {
	return page.getByTestId('queue-announcement');
}

async function queueOrder(page: Page): Promise<(string | null)[]> {
	return page
		.getByTestId('queue-item')
		.evaluateAll((items) =>
			items.map((item) => item.getAttribute('data-track-id') ?? item.getAttribute('data-id'))
		);
}

async function ariaCurrent(locator: Locator): Promise<string | null> {
	return locator.evaluate((element) => element.getAttribute('aria-current'));
}

async function activeIdentity(page: Page): Promise<{
	tag: string;
	label: string;
	testid: string;
	track: string;
}> {
	return page.evaluate(() => {
		const el = document.activeElement as HTMLElement | null;
		const item = el?.closest?.('[data-testid="queue-item"]');
		return {
			tag: el?.tagName ?? '',
			label: (el?.getAttribute('aria-label') ?? el?.textContent ?? '').trim().slice(0, 80),
			testid: el?.getAttribute('data-testid') ?? '',
			track: item?.getAttribute('data-track-id') ?? item?.getAttribute('data-id') ?? ''
		};
	});
}

/** Fill the queue in gesture order: mike, zulu, alpha. Current is mike. */
async function seedQueue(
	page: Page,
	payload: { current: unknown } = { current: ITEMS }
): Promise<void> {
	await seedLibrary(page, payload);
	await row(page, 'row-mike').click();
	await row(page, 'row-zulu').click();
	await row(page, 'row-alpha').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
}

/** The recovered control is a usable native button, not a new tab stop. */
async function expectUsableRemove(target: Locator): Promise<void> {
	await expect(target).toBeFocused();
	await expect(target).toBeVisible();
	await expect(target).toHaveAttribute('type', 'button');
	expect(await target.getAttribute('tabindex')).toBeNull();
	await expect(target).toBeEnabled();
}

/** The empty-queue target is focused, visible and programmatically focusable. */
async function expectEmptyRecovery(page: Page): Promise<void> {
	const target = emptyTarget(page);
	await expect(target).toBeFocused();
	await expect(target).toBeVisible();
	await expect(target).toHaveAttribute('tabindex', '-1');
	const tag = await target.evaluate((el) => el.tagName);
	expect(tag).not.toMatch(/^(BUTTON|A|INPUT|SELECT|TEXTAREA)$/);
	const focus = await target.evaluate((el) => {
		const css = getComputedStyle(el);
		return {
			matches: el.matches(':focus-visible'),
			ring:
				(css.outlineStyle !== 'none' && parseFloat(css.outlineWidth) > 0) ||
				css.boxShadow !== 'none'
		};
	});
	expect(focus.matches).toBe(true);
	expect(focus.ring).toBe(true);
	expect((await target.textContent())?.trim() ?? '').toMatch(/queue|empty|clear/i);
	const identity = await activeIdentity(page);
	expect(identity.tag).not.toBe('BODY');
}

/** Record one entry per live-region commit so "announced once" is countable. */
async function watchAnnouncements(page: Page): Promise<void> {
	await page.evaluate(() => {
		const region = document.querySelector('[data-testid="queue-announcement"]');
		const watched = window as unknown as {
			__qfCommits: string[];
			__qfObserver?: MutationObserver;
		};
		// A second call must replace, not stack onto, the first observer:
		// two live observers on the same node would push one commit twice.
		watched.__qfObserver?.disconnect();
		watched.__qfCommits = [];
		if (!region) return;
		watched.__qfObserver = new MutationObserver(() => {
			watched.__qfCommits.push(region.textContent ?? '');
		});
		watched.__qfObserver.observe(region, { childList: true, characterData: true, subtree: true });
	});
}

async function announcementCommits(page: Page): Promise<string[]> {
	return page.evaluate(
		() => (window as unknown as { __qfCommits?: string[] }).__qfCommits ?? []
	);
}

test('QF-B00 control: the mocked harness seeds the queue and moves announce', async ({
	page
}) => {
	await seedQueue(page);

	const moveUp = rowButton(page, 'row-alpha', 'Move Alpha Song up');
	await expect(moveUp).toBeVisible();
	await moveUp.click();

	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha', 'row-zulu']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
	await expect(announcement(page)).toContainText('Alpha Song');
	await expect(announcement(page)).toContainText('2 of 3');
	await expect(announcement(page)).toHaveAttribute('role', 'status');
	expect(await ariaCurrent(queueItem(page, 'row-mike'))).toBe('true');
});

test('QF-B01 removing the middle row focuses the next surviving Remove', async ({ page }) => {
	await seedQueue(page);

	const remove = rowButton(page, 'row-zulu', 'Remove Zulu Song from queue');
	await expect(remove).toBeVisible();
	await remove.focus();
	await expect(remove).toBeFocused();
	await page.keyboard.press('Enter');

	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
	expect(await ariaCurrent(queueItem(page, 'row-mike'))).toBe('true');
	await expectUsableRemove(rowButton(page, 'row-alpha', 'Remove Alpha Song from queue'));
	const identity = await activeIdentity(page);
	expect(identity.track).toBe('row-alpha');
	expect(identity.tag).not.toBe('BODY');
});

test('QF-B02 removing the last row focuses the previous surviving Remove', async ({ page }) => {
	await seedQueue(page);

	const remove = rowButton(page, 'row-alpha', 'Remove Alpha Song from queue');
	await expect(remove).toBeVisible();
	await remove.focus();
	await page.keyboard.press('Enter');

	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
	await expectUsableRemove(rowButton(page, 'row-zulu', 'Remove Zulu Song from queue'));
	const identity = await activeIdentity(page);
	expect(identity.track).toBe('row-zulu');
	expect(identity.tag).not.toBe('BODY');
});

test('QF-B03 removing the first row when it is not current focuses the next Remove', async ({
	page
}) => {
	await seedQueue(page);
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');

	const remove = rowButton(page, 'row-mike', 'Remove Mike Song from queue');
	await expect(remove).toBeVisible();
	await remove.focus();
	await page.keyboard.press('Enter');

	expect(await queueOrder(page)).toEqual(['row-zulu', 'row-alpha']);
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');
	expect(await ariaCurrent(queueItem(page, 'row-zulu'))).toBe('true');
	await expectUsableRemove(rowButton(page, 'row-zulu', 'Remove Zulu Song from queue'));
	expect((await activeIdentity(page)).tag).not.toBe('BODY');
});

test('QF-B04 removing the current middle row advances playback and focuses next Remove', async ({
	page
}) => {
	await seedQueue(page);
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');

	const remove = rowButton(page, 'row-zulu', 'Remove Zulu Song from queue');
	await expect(remove).toBeVisible();
	await remove.focus();
	await page.keyboard.press('Enter');

	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha']);
	await expect(page.getByTestId('now-playing')).toContainText('Alpha Song');
	expect(await ariaCurrent(queueItem(page, 'row-alpha'))).toBe('true');
	expect(await ariaCurrent(queueItem(page, 'row-mike'))).toBeNull();
	await expect(page.getByTestId('player-next')).toBeEnabled();
	await expectUsableRemove(rowButton(page, 'row-alpha', 'Remove Alpha Song from queue'));
	expect((await activeIdentity(page)).tag).not.toBe('BODY');
});

test('QF-B05 removing the current tail falls back and focuses the previous Remove', async ({
	page
}) => {
	await seedQueue(page);
	await page.getByTestId('player-next').click();
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Alpha Song');

	const remove = rowButton(page, 'row-alpha', 'Remove Alpha Song from queue');
	await expect(remove).toBeVisible();
	await remove.focus();
	await page.keyboard.press('Enter');

	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu']);
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');
	expect(await ariaCurrent(queueItem(page, 'row-zulu'))).toBe('true');
	await expectUsableRemove(rowButton(page, 'row-zulu', 'Remove Zulu Song from queue'));
	expect((await activeIdentity(page)).tag).not.toBe('BODY');
});

test('QF-B06 removing the sole row focuses the stable empty target', async ({ page }) => {
	await seedLibrary(page);
	await row(page, 'row-mike').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);

	const remove = rowButton(page, 'row-mike', 'Remove Mike Song from queue');
	await expect(remove).toBeVisible();
	await remove.focus();
	await page.keyboard.press('Enter');

	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
	await expect(page.getByTestId('player-next')).toBeDisabled();
	await expect(page.getByRole('button', { name: 'Clear queue', exact: true })).toBeDisabled();
	await expectEmptyRecovery(page);
});

test('QF-B07 focused Clear empties the queue onto the empty target', async ({ page }) => {
	await seedQueue(page);

	const clear = page.getByRole('button', { name: 'Clear queue', exact: true });
	await expect(clear).toBeVisible();
	await expect(clear).toBeEnabled();
	await clear.focus();
	await page.keyboard.press('Enter');

	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	expect(await queueOrder(page)).toEqual([]);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
	await expect(page.getByTestId('player-next')).toBeDisabled();
	await expect(clear).toBeDisabled();
	await expect(clear).not.toBeFocused();
	await expectEmptyRecovery(page);
});

test('QF-B08 consecutive keyboard removals stay operable until the queue empties', async ({
	page
}) => {
	await seedQueue(page);

	const first = rowButton(page, 'row-zulu', 'Remove Zulu Song from queue');
	await expect(first).toBeVisible();
	await first.focus();
	await page.keyboard.press('Enter');

	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha']);
	await expectUsableRemove(rowButton(page, 'row-alpha', 'Remove Alpha Song from queue'));
	await page.keyboard.press('Enter');

	expect(await queueOrder(page)).toEqual(['row-mike']);
	await expectUsableRemove(rowButton(page, 'row-mike', 'Remove Mike Song from queue'));
	await page.keyboard.press('Enter');

	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	expect(await queueOrder(page)).toEqual([]);
	await expectEmptyRecovery(page);
	// Negative: the queue never repopulated and transport stayed off with it.
	await expect(page.getByTestId('player-next')).toBeDisabled();
	await expect(page.getByTestId('now-playing')).not.toContainText('Mike Song');
});

test('QF-B09 the empty target is inert: activating it plays, seeks and repopulates nothing', async ({
	page
}) => {
	await seedQueue(page);

	const clear = page.getByRole('button', { name: 'Clear queue', exact: true });
	await expect(clear).toBeVisible();
	await clear.focus();
	await page.keyboard.press('Enter');
	await expectEmptyRecovery(page);

	await page.keyboard.press('Enter');
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	expect(await queueOrder(page)).toEqual([]);
	await expectEmptyRecovery(page);
	await expect(page.getByTestId('player-next')).toBeDisabled();
	await expect(page.locator('audio')).toHaveCount(0);
});

test('QF-B10 regression: move focus and current-row reveal keep working', async ({ page }) => {
	await seedQueue(page);

	const moveAlpha = rowButton(page, 'row-alpha', 'Move Alpha Song up');
	await expect(moveAlpha).toBeVisible();
	await moveAlpha.focus();
	await expect(moveAlpha).toBeFocused();
	await page.keyboard.press('Enter');

	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha', 'row-zulu']);
	await expect(moveAlpha).toBeFocused();
	await expect(announcement(page)).toContainText('Alpha Song');
	await expect(announcement(page)).toContainText('2 of 3');

	const reveal = page.getByRole('button', { name: 'Show current track', exact: true });
	await expect(reveal).toBeEnabled();
	await reveal.click();
	await expect(queueItem(page, 'row-mike')).toBeFocused();
	expect(await ariaCurrent(queueItem(page, 'row-mike'))).toBe('true');
});

test('QF-B11 a removal that never held focus leaves focus alone', async ({ page }) => {
	await seedQueue(page);

	const next = page.getByTestId('player-next');
	await next.focus();
	await expect(next).toBeFocused();
	// dispatchEvent clicks without moving focus: the removal is real, but the
	// focused control is not the doomed one, so no recovery may fire.
	await rowButton(page, 'row-zulu', 'Remove Zulu Song from queue').dispatchEvent('click');

	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha']);
	await expect(next).toBeFocused();
	const identity = await activeIdentity(page);
	expect(identity.tag).not.toBe('BODY');
	expect(identity.track).toBe('');
});

test('QF-B12 a newer Tab movement wins over an older removal focus', async ({ page }) => {
	await seedQueue(page);

	const doomed = rowButton(page, 'row-mike', 'Remove Mike Song from queue');
	await expect(doomed).toBeVisible();
	await doomed.focus();
	// Two Tabs: the first stop (zulu Up) is disabled by the removal itself,
	// which would strand focus regardless of recovery. The second stop stays
	// enabled, so it pins the recency rule instead of disabled-control fallout.
	await page.keyboard.press('Tab');
	await page.keyboard.press('Tab');
	const identity = await activeIdentity(page);
	expect(identity.tag).toBe('BUTTON');
	expect(identity.label).toBe('Move Zulu Song down');
	const newer = page.getByRole('button', { name: identity.label, exact: true });
	await expect(newer).toBeFocused();

	await doomed.dispatchEvent('click');

	expect(await queueOrder(page)).toEqual(['row-zulu', 'row-alpha']);
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');
	await expect(newer).toBeFocused();
	expect((await activeIdentity(page)).tag).not.toBe('BODY');
});

test('QF-B13 a background refresh that drops a queued id moves focus nowhere into the queue', async ({
	page
}) => {
	const payload: { current: unknown } = { current: ITEMS };
	await seedLibrary(page, payload);
	await row(page, 'row-mike').click();
	await row(page, 'row-zulu').click();
	await row(page, 'row-alpha').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(3);

	const next = page.getByTestId('player-next');
	await next.focus();
	payload.current = [ITEMS[0], ITEMS[2]];
	// dispatchEvent triggers the refresh without moving focus to the Refresh
	// button the way a pointer click would.
	await page.getByRole('button', { name: 'Refresh', exact: true }).dispatchEvent('click');

	await expect.poll(() => queueOrder(page)).toEqual(['row-mike', 'row-zulu']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
	// The refresh remounts the panel through the loading branch, so pre-refresh
	// focus is honestly lost with the old nodes; the pin is that no recovery
	// logic yanked focus back INTO the queue afterwards.
	const inQueue = await page.evaluate(() => {
		const el = document.activeElement as HTMLElement | null;
		return Boolean(el?.closest?.('[data-testid="queue-item"], [data-testid="queue-empty-focus"]'));
	});
	expect(inQueue).toBe(false);
	// Negative: the survivors stay usable transport-wise, not just listed.
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');
});

test('QF-B14 an auth purge clears the queue without routing focus into it', async ({ page }) => {
	const payload: { current: unknown } = { current: ITEMS };
	await seedQueue(page, payload);

	const refresh = page.getByRole('button', { name: 'Refresh', exact: true });
	await refresh.focus();
	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate((key) => {
		window.localStorage.setItem(key, 'test-auth-transition');
		window.localStorage.removeItem(key);
	}, 'nntune.library.auth-transition.v1');
	await otherTab.close();

	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	// The queue panel unmounts with the library: no recovery target may exist
	// and focus must not have been yanked into the removed panel.
	await expect(emptyTarget(page)).toHaveCount(0);
	await expect(page.getByTestId('queue-announcement')).toHaveCount(0);
	payload.current = [{ id: 'row-bravo', title: 'Bravo Song' }];
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expect(page.getByTestId('library-list')).toContainText('Bravo Song');
});

test('QF-B15 narrow viewport: removal recovery stays visible and operable', async ({ page }) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await seedQueue(page);

	const remove = rowButton(page, 'row-zulu', 'Remove Zulu Song from queue');
	await expect(remove).toBeVisible();
	await remove.focus();
	await page.keyboard.press('Enter');

	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha']);
	const recovered = rowButton(page, 'row-alpha', 'Remove Alpha Song from queue');
	await expectUsableRemove(recovered);
	await expect(recovered).toBeInViewport();
	expect((await activeIdentity(page)).tag).not.toBe('BODY');
});

for (const scheme of ['light', 'dark'] as const) {
	test(`QF-B1${scheme === 'light' ? '6' : '7'} ${scheme} scheme: removal recovery works identically`, async ({
		page
	}) => {
		await page.emulateMedia({ colorScheme: scheme });
		await seedQueue(page);

		const remove = rowButton(page, 'row-zulu', 'Remove Zulu Song from queue');
		await expect(remove).toBeVisible();
		await remove.focus();
		await page.keyboard.press('Enter');

		expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha']);
		await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
		await expectUsableRemove(rowButton(page, 'row-alpha', 'Remove Alpha Song from queue'));
		expect((await activeIdentity(page)).tag).not.toBe('BODY');

		const clear = page.getByRole('button', { name: 'Clear queue', exact: true });
		await clear.focus();
		await page.keyboard.press('Enter');
		await expect(page.getByTestId('queue-item')).toHaveCount(0);
		await expectEmptyRecovery(page);
	});
}

test('QF-B18 remove and clear announce exactly once without replaying moves', async ({ page }) => {
	await seedQueue(page);
	await expect(announcement(page)).toHaveAttribute('role', 'status');
	await expect(page.getByTestId('queue-announcement')).toHaveCount(1);

	await rowButton(page, 'row-alpha', 'Move Alpha Song up').click();
	await expect(announcement(page)).toContainText('moved to 2 of 3');

	const remove = rowButton(page, 'row-zulu', 'Remove Zulu Song from queue');
	await expect(remove).toBeVisible();
	await watchAnnouncements(page);
	await remove.focus();
	await page.keyboard.press('Enter');

	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha']);
	await expect(announcement(page)).toContainText('Zulu Song');
	await expect(announcement(page)).not.toContainText('moved to');
	await expect
		.poll(() => announcementCommits(page), { message: 'remove announces exactly once' })
		.toHaveLength(1);
	await page.waitForTimeout(150);
	expect(await announcementCommits(page)).toHaveLength(1);
	await expect(announcement(page)).toContainText('Zulu Song');

	const clear = page.getByRole('button', { name: 'Clear queue', exact: true });
	await watchAnnouncements(page);
	await clear.focus();
	await page.keyboard.press('Enter');

	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(announcement(page)).not.toHaveText('');
	await expect(announcement(page)).not.toContainText('moved to');
	await expect(announcement(page)).toContainText(/clear|empty|removed/i);
	await expect
		.poll(() => announcementCommits(page), { message: 'clear announces exactly once' })
		.toHaveLength(1);
});

test('QF-B19 transport semantics survive removals without playback side effects', async ({
	page
}) => {
	await seedQueue(page);

	const remove = rowButton(page, 'row-zulu', 'Remove Zulu Song from queue');
	await expect(remove).toBeVisible();
	await remove.focus();
	await page.keyboard.press('Enter');

	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
	expect(await ariaCurrent(queueItem(page, 'row-mike'))).toBe('true');
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Alpha Song');
	expect(await ariaCurrent(queueItem(page, 'row-alpha'))).toBe('true');
	// Negative: recovery adds no rows, starts no media and seeks nowhere.
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	await expect(page.locator('audio')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).not.toContainText('Mike Song');
});
