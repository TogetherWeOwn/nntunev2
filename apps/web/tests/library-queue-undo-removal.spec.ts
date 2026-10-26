import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * UNDO — protected independent acceptance (host/browser layer), paired with
 * src/lib/player/queue.undoRemoval.acceptance.test.ts. Keep both files
 * unchanged in the combined implementation PR; return incorrect tests to
 * their author, never silently weaken them. No standalone ready PR of this
 * intentionally RED bank. Bank ref: test/queue-undo-removal-acceptance.
 * Baseline: a242bbc721f17e736757dadc4a6f85ba396e2d3f.
 *
 * Locked contract on the real /library route (the demo player is out of
 * scope):
 * - After a single-track removal through a row's `Remove {title} from queue`
 *   button, exactly one undo affordance is offered: a native button with the
 *   exact accessible name `Undo removal of {title}`, type="button", enabled,
 *   with no tabindex attribute (a natural tab stop). Before any removal, after
 *   a successful undo, and after the slot retires it is absent or disabled —
 *   never an enabled control that does nothing.
 * - Undo restores the exact removed track at its original index and the
 *   pre-removal selection. For the current track that means the restored
 *   track is current again (now-playing / aria-current), as metadata only:
 *   undo never plays, pauses, loads, seeks, rewrites a media source or calls
 *   an API. /library has no audio element; the media probe catches accidental
 *   transport. These tests do not claim real audio evidence.
 * - One slot, single use: a second removal replaces the offer (only the
 *   latest track can return). Any other queue mutation — append (including
 *   re-adding the removed track), reorder, clear, track change, a library
 *   refresh that drops a queued track, an auth reset — retires the offer, and
 *   nothing afterwards restores the track or claims it did. Clear itself
 *   offers no undo (undo for clear-all is guarded elsewhere).
 * - Announcements reuse the existing `queue-announcement` polite region in
 *   `main` (role="status", never aria-live="assertive"; page-wide assertive
 *   counts are meaningless because of the SvelteKit announcer, so live-region
 *   checks are scoped to `main`). A successful undo announces the track title
 *   plus the word "restored" exactly once; a retired slot produces no
 *   "restored" copy.
 * - Focus: offering undo steals nothing (the shipped removal focus-recovery
 *   target keeps focus). A keyboard-activated undo moves focus to the
 *   restored row's `Remove {title} from queue` control.
 * - Session-only: after a reload no undo is offered, whatever the queue does.
 *
 * The pure restore/fingerprint semantics (exact track object, original
 * index, identical-state no-ops) are pinned in the companion unit bank; this
 * spec pins the offer/retire/focus/announcement behaviour a user can see.
 * Breaks caught: undo appending at the end, restoring the wrong track,
 * resurrecting the first of two removals, undo surviving append/reorder/
 * clear/track-change/refresh/auth, false "restored" announcements, focus
 * stranded on the vanished undo button, transport/API side effects.
 *
 * Harness note (same as neighbouring library specs): /library is
 * client-fetched (ssr = false) with a route-mocked /api/library. Every test
 * guards its first undo interaction with an explicit visibility assertion,
 * so a build without the affordance fails as an assertion, never a click
 * error. Existing hooks only: queue-item/data-track-id, now-playing,
 * player-next, queue-announcement, queue-empty-focus.
 *
 * Reproduce from apps/web (Node 24 / pnpm 10):
 *   pnpm install --frozen-lockfile
 *   pnpm test src/lib/player/queue.undoRemoval.acceptance.test.ts
 *   CI=1 PUBLIC_API_BASE_URL='' pnpm exec playwright test tests/library-queue-undo-removal.spec.ts --workers=1
 * Regressions: tests/library-queue-remove-focus.spec.ts,
 * tests/library-queue-manage.spec.ts, tests/library-queue-announcement.spec.ts
 * and src/lib/player/playerQueue.removeFocus.test.ts; plus pnpm test and
 * pnpm check. CI=1 forces this run's own preview server on 4173, never
 * another worktree's. Expected on the baseline: UNDO-B00/B12 controls pass,
 * the feature cases fail on the missing undo affordance assertion; no
 * import/build/harness errors. Post-removal assertions after that first
 * failure point remain unverified until implementation.
 */

test.use({ viewport: { width: 1000, height: 700 } });

// API order; queue order comes from the click sequence in seedQueue.
const ITEMS = [
	{ id: 'row-zulu', title: 'Zulu Song', artist: 'Queue Artist' },
	{ id: 'row-alpha', title: 'Alpha Song' },
	{ id: 'row-mike', title: 'Mike Song', artist: 'Queue Artist' },
	{ id: 'row-bravo', title: 'Bravo Song' }
];
const TITLES: Record<string, string> = Object.fromEntries(ITEMS.map((i) => [i.id, i.title]));

async function seedLibrary(
	page: Page,
	payload: { current: unknown } = { current: ITEMS },
	counter?: { fetches: number }
): Promise<void> {
	await page.route('**/api/library', (route) => {
		if (counter) counter.fetches += 1;
		return route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(payload.current)
		});
	});
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(
		(payload.current as unknown[]).length
	);
}

/** Queue order: mike, zulu, alpha, bravo (click order). Current is mike. */
async function seedQueue(
	page: Page,
	payload: { current: unknown } = { current: ITEMS },
	clickIds: string[] = ['row-mike', 'row-zulu', 'row-alpha', 'row-bravo'],
	counter?: { fetches: number }
): Promise<void> {
	await seedLibrary(page, payload, counter);
	for (const id of clickIds) await row(page, id).click();
	await expect(page.getByTestId('queue-item')).toHaveCount(clickIds.length);
	await expect(page.getByTestId('now-playing')).toContainText(TITLES[clickIds[0]!]!);
	expect(await queueOrder(page)).toEqual(clickIds);
}

function row(page: Page, id: string): Locator {
	return page.locator(`[data-testid="library-item"][data-id="${id}"]`);
}

function queueItem(page: Page, id: string): Locator {
	return page.locator(`[data-testid="queue-item"][data-track-id="${id}"]`);
}

function rowButton(page: Page, trackId: string, name: string): Locator {
	return queueItem(page, trackId).getByRole('button', { name, exact: true });
}

function removeButton(page: Page, trackId: string): Locator {
	return rowButton(page, trackId, `Remove ${TITLES[trackId]} from queue`);
}

function undoButton(page: Page, trackId: string): Locator {
	return page.getByRole('button', { name: `Undo removal of ${TITLES[trackId]}`, exact: true });
}

function anyUndoButton(page: Page): Locator {
	return page.getByRole('button', { name: /^Undo removal of / });
}

function announcement(page: Page): Locator {
	return page.getByTestId('queue-announcement');
}

async function queueOrder(page: Page): Promise<(string | null)[]> {
	return page
		.getByTestId('queue-item')
		.evaluateAll((items) => items.map((item) => item.getAttribute('data-track-id')));
}

async function ariaCurrent(locator: Locator): Promise<string | null> {
	return locator.evaluate((element) => element.getAttribute('aria-current'));
}

/** The offer is live: one native, enabled, naturally tabbable undo button. */
async function expectUndoOffered(page: Page, trackId: string): Promise<Locator> {
	const undo = undoButton(page, trackId);
	await expect(undo, `undo offer for ${TITLES[trackId]} must be visible`).toBeVisible();
	await expect(undo).toBeEnabled();
	await expect(undo).toHaveAttribute('type', 'button');
	expect(await undo.getAttribute('tabindex')).toBeNull();
	await expect(anyUndoButton(page)).toHaveCount(1);
	return undo;
}

/** The slot is gone: no enabled undo affordance exists for this track. */
async function expectUndoRetired(page: Page, trackId: string): Promise<void> {
	const undo = undoButton(page, trackId);
	if ((await undo.count()) === 0) return;
	await expect(undo).toBeDisabled();
}

/** No track at all may be undone right now. */
async function expectNoUndoOffered(page: Page): Promise<void> {
	for (const item of ITEMS) await expectUndoRetired(page, item.id);
}

async function advanceTo(page: Page, trackId: string, clicks: number): Promise<void> {
	for (let i = 0; i < clicks; i += 1) await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText(TITLES[trackId]!);
	expect(await ariaCurrent(queueItem(page, trackId))).toBe('true');
}

/** Record one entry per live-region commit so "announced once" is countable. */
async function watchAnnouncements(page: Page): Promise<void> {
	await page.evaluate(() => {
		const region = document.querySelector('[data-testid="queue-announcement"]');
		const watched = window as unknown as {
			__undoCommits: string[];
			__undoObserver?: MutationObserver;
		};
		watched.__undoObserver?.disconnect();
		watched.__undoCommits = [];
		if (!region) return;
		watched.__undoObserver = new MutationObserver(() => {
			watched.__undoCommits.push(region.textContent ?? '');
		});
		watched.__undoObserver.observe(region, { childList: true, characterData: true, subtree: true });
	});
}

async function announcementCommits(page: Page): Promise<string[]> {
	return page.evaluate(
		() => (window as unknown as { __undoCommits?: string[] }).__undoCommits ?? []
	);
}

/** Undo and pin the full restored state: order, selection, single use. */
async function undoAndExpect(
	page: Page,
	trackId: string,
	wantOrder: string[],
	wantCurrent: string
): Promise<void> {
	const undo = await expectUndoOffered(page, trackId);
	await undo.click();

	await expect.poll(() => queueOrder(page)).toEqual(wantOrder);
	await expect(page.getByTestId('now-playing')).toContainText(TITLES[wantCurrent]!);
	expect(await ariaCurrent(queueItem(page, wantCurrent))).toBe('true');
	await expect(announcement(page)).toContainText(TITLES[trackId]!);
	await expect(announcement(page)).toContainText(/restored/i);
	await expectUndoRetired(page, trackId);
}

test('UNDO-B00 control: a seeded queue offers no undo before any removal', async ({ page }) => {
	await seedQueue(page);

	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu', 'row-alpha', 'row-bravo']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
	await expectNoUndoOffered(page);
	await expect(removeButton(page, 'row-zulu')).toBeVisible();
});

test('UNDO-B01 removing a middle upcoming track then undoing restores its original index', async ({
	page
}) => {
	await seedQueue(page);

	await removeButton(page, 'row-alpha').click();
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu', 'row-bravo']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
	// The removal announcement must not already claim a restore.
	await expect(announcement(page)).toContainText('Alpha Song');
	await expect(announcement(page)).not.toContainText(/restored/i);

	await undoAndExpect(page, 'row-alpha', ['row-mike', 'row-zulu', 'row-alpha', 'row-bravo'], 'row-mike');
});

test('UNDO-B02 removing the first track before the current one then undoing keeps the selection', async ({
	page
}) => {
	await seedQueue(page);
	await advanceTo(page, 'row-alpha', 2);

	await removeButton(page, 'row-mike').click();
	expect(await queueOrder(page)).toEqual(['row-zulu', 'row-alpha', 'row-bravo']);
	await expect(page.getByTestId('now-playing')).toContainText('Alpha Song');

	await undoAndExpect(page, 'row-mike', ['row-mike', 'row-zulu', 'row-alpha', 'row-bravo'], 'row-alpha');
});

test('UNDO-B03 removing the last track then undoing restores it at the end', async ({ page }) => {
	await seedQueue(page);

	await removeButton(page, 'row-bravo').click();
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu', 'row-alpha']);

	await undoAndExpect(page, 'row-bravo', ['row-mike', 'row-zulu', 'row-alpha', 'row-bravo'], 'row-mike');
});

test('UNDO-B04 removing the current middle track then undoing makes it current again', async ({
	page
}) => {
	await seedQueue(page);
	await advanceTo(page, 'row-zulu', 1);

	await removeButton(page, 'row-zulu').click();
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha', 'row-bravo']);
	await expect(page.getByTestId('now-playing')).toContainText('Alpha Song');

	await undoAndExpect(page, 'row-zulu', ['row-mike', 'row-zulu', 'row-alpha', 'row-bravo'], 'row-zulu');
});

test('UNDO-B05 removing the current tail then undoing restores it as the current tail', async ({
	page
}) => {
	await seedQueue(page);
	await advanceTo(page, 'row-bravo', 3);

	await removeButton(page, 'row-bravo').click();
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu', 'row-alpha']);
	await expect(page.getByTestId('now-playing')).toContainText('Alpha Song');

	await undoAndExpect(page, 'row-bravo', ['row-mike', 'row-zulu', 'row-alpha', 'row-bravo'], 'row-bravo');
});

test('UNDO-B06 removing the sole track then undoing restores the one-track queue', async ({
	page
}) => {
	await seedQueue(page, { current: ITEMS }, ['row-mike']);

	await removeButton(page, 'row-mike').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');

	await undoAndExpect(page, 'row-mike', ['row-mike'], 'row-mike');
});

test('UNDO-B07 a second removal replaces the slot: only the latest track returns', async ({
	page
}) => {
	await seedQueue(page);

	await removeButton(page, 'row-zulu').click();
	await expectUndoOffered(page, 'row-zulu');
	await removeButton(page, 'row-alpha').click();
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-bravo']);
	await expectUndoRetired(page, 'row-zulu');

	await undoAndExpect(page, 'row-alpha', ['row-mike', 'row-alpha', 'row-bravo'], 'row-mike');
	// The first removal stays removed and no further undo is offered.
	expect(await queueOrder(page)).not.toContain('row-zulu');
	await expectNoUndoOffered(page);
});

test('UNDO-B08 appending a track after a removal retires the undo offer', async ({ page }) => {
	await seedQueue(page, { current: ITEMS }, ['row-mike', 'row-zulu', 'row-alpha']);

	await removeButton(page, 'row-zulu').click();
	await expectUndoOffered(page, 'row-zulu');
	await row(page, 'row-bravo').click();

	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha', 'row-bravo']);
	await expectUndoRetired(page, 'row-zulu');
	await expect(announcement(page)).not.toContainText(/restored/i);
	expect(await queueOrder(page)).not.toContain('row-zulu');
});

test('UNDO-B09 re-adding the removed track retires undo and never duplicates it', async ({
	page
}) => {
	await seedQueue(page, { current: ITEMS }, ['row-mike', 'row-zulu', 'row-alpha']);

	await removeButton(page, 'row-zulu').click();
	await expectUndoOffered(page, 'row-zulu');
	await row(page, 'row-zulu').click();

	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha', 'row-zulu']);
	await expectUndoRetired(page, 'row-zulu');
	await expect(announcement(page)).not.toContainText(/restored/i);
});

test('UNDO-B10 reordering survivors after a removal retires the undo offer', async ({ page }) => {
	await seedQueue(page);

	await removeButton(page, 'row-alpha').click();
	await expectUndoOffered(page, 'row-alpha');
	await rowButton(page, 'row-bravo', 'Move Bravo Song up').click();

	expect(await queueOrder(page)).toEqual(['row-mike', 'row-bravo', 'row-zulu']);
	await expectUndoRetired(page, 'row-alpha');
	await expect(announcement(page)).not.toContainText(/restored/i);
	expect(await queueOrder(page)).not.toContain('row-alpha');
});

test('UNDO-B11 clearing after a removal retires the offer and restores nothing', async ({
	page
}) => {
	await seedQueue(page);

	await removeButton(page, 'row-zulu').click();
	await expectUndoOffered(page, 'row-zulu');
	await page.getByRole('button', { name: 'Clear queue', exact: true }).click();

	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
	await expectUndoRetired(page, 'row-zulu');
	await expect(announcement(page)).not.toContainText(/restored/i);
});

test('UNDO-B12 control: clearing without a prior single removal offers no undo', async ({
	page
}) => {
	await seedQueue(page);

	await page.getByRole('button', { name: 'Clear queue', exact: true }).click();

	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expectNoUndoOffered(page);
});

test('UNDO-B13 changing the current track after a removal retires the undo offer', async ({
	page
}) => {
	await seedQueue(page);

	await removeButton(page, 'row-bravo').click();
	await expectUndoOffered(page, 'row-bravo');
	await advanceTo(page, 'row-zulu', 1);

	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu', 'row-alpha']);
	await expectUndoRetired(page, 'row-bravo');
	await expect(announcement(page)).not.toContainText(/restored/i);
	expect(await queueOrder(page)).not.toContain('row-bravo');
});

test('UNDO-B14 an auth reset discards the queue and its undo offer for the next account', async ({
	page
}) => {
	const payload: { current: unknown } = { current: ITEMS };
	await seedQueue(page, payload);

	await removeButton(page, 'row-zulu').click();
	await expectUndoOffered(page, 'row-zulu');

	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate((key) => {
		window.localStorage.setItem(key, 'test-auth-transition');
		window.localStorage.removeItem(key);
	}, 'nntune.library.auth-transition.v1');
	await otherTab.close();

	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(anyUndoButton(page)).toHaveCount(0);

	payload.current = [{ id: 'row-next', title: 'Next Account Song' }];
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expect(page.getByTestId('library-list')).toContainText('Next Account Song');
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(anyUndoButton(page)).toHaveCount(0);
});

test('UNDO-B15 a library refresh that drops a queued survivor retires the undo offer', async ({
	page
}) => {
	const payload: { current: unknown } = { current: ITEMS };
	await seedQueue(page, payload);

	await removeButton(page, 'row-zulu').click();
	await expectUndoOffered(page, 'row-zulu');

	payload.current = ITEMS.filter((item) => item.id !== 'row-alpha');
	// dispatchEvent triggers the refresh without any pointer focus move.
	await page.getByRole('button', { name: 'Refresh', exact: true }).dispatchEvent('click');

	await expect.poll(() => queueOrder(page)).toEqual(['row-mike', 'row-bravo']);
	await expectUndoRetired(page, 'row-zulu');
	await expect(announcement(page)).not.toContainText(/restored/i);
	expect(await queueOrder(page)).not.toContain('row-zulu');
});

test('UNDO-B16 keyboard undo lands focus on the restored row Remove control, stealing nothing first', async ({
	page
}) => {
	await seedQueue(page);

	const remove = removeButton(page, 'row-alpha');
	await expect(remove).toBeVisible();
	await remove.focus();
	await page.keyboard.press('Enter');
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu', 'row-bravo']);

	// The offer appears without stealing the shipped removal recovery focus.
	const undo = await expectUndoOffered(page, 'row-alpha');
	await expect(removeButton(page, 'row-bravo')).toBeFocused();
	await expect(undo).not.toBeFocused();

	await undo.focus();
	await page.keyboard.press('Enter');

	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu', 'row-alpha', 'row-bravo']);
	const restored = removeButton(page, 'row-alpha');
	await expect(restored).toBeFocused();
	await expect(restored).toBeVisible();
	await expect(restored).toBeEnabled();
	await expect(restored).toHaveAttribute('type', 'button');
	expect(await restored.getAttribute('tabindex')).toBeNull();
	const tag = await page.evaluate(() => document.activeElement?.tagName ?? '');
	expect(tag).not.toBe('BODY');
});

test('UNDO-B17 undo announces once through the polite region scoped to main', async ({ page }) => {
	await seedQueue(page);

	await removeButton(page, 'row-zulu').click();
	const undo = await expectUndoOffered(page, 'row-zulu');

	const region = page.locator('main [data-testid="queue-announcement"]');
	await expect(region).toHaveCount(1);
	await expect(region).toHaveAttribute('role', 'status');
	await expect(region).not.toHaveAttribute('aria-live', 'assertive');
	await expect(page.locator('main [aria-live="assertive"]')).toHaveCount(0);

	await watchAnnouncements(page);
	await undo.click();

	await expect(region).toContainText('Zulu Song');
	await expect(region).toContainText(/restored/i);
	expect(((await region.textContent()) ?? '').trim().length).toBeLessThanOrEqual(160);
	await expect
		.poll(() => announcementCommits(page), { message: 'undo announces exactly once' })
		.toHaveLength(1);
	await page.waitForTimeout(150);
	expect(await announcementCommits(page)).toHaveLength(1);
	await expect(page.locator('main [aria-live="assertive"]')).toHaveCount(0);
});

test('UNDO-B18 no undo is offered after a reload (session-only)', async ({ page }) => {
	await seedQueue(page);

	await removeButton(page, 'row-zulu').click();
	await expectUndoOffered(page, 'row-zulu');

	await page.reload();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);

	await expect(anyUndoButton(page)).toHaveCount(0);
	// Whatever the queue does across reload, undo never resurrects the track:
	// the offer that could restore it is gone.
	await expectUndoRetired(page, 'row-zulu');
});

test('UNDO-B19 undoing a current-track removal causes no transport or API side effects', async ({
	page
}) => {
	await page.addInitScript(() => {
		const probe = { media: [] as string[], positions: [] as number[], sources: [] as string[] };
		(window as unknown as { undoProbe: typeof probe }).undoProbe = probe;
		for (const method of ['play', 'pause', 'load'] as const) {
			Object.defineProperty(HTMLMediaElement.prototype, method, {
				configurable: true,
				value() {
					probe.media.push(method);
					return method === 'play' ? Promise.resolve() : undefined;
				}
			});
		}
		for (const property of ['currentTime', 'src'] as const) {
			const descriptor = Object.getOwnPropertyDescriptor(HTMLMediaElement.prototype, property)!;
			Object.defineProperty(HTMLMediaElement.prototype, property, {
				...descriptor,
				set(value: number | string) {
					if (property === 'currentTime') probe.positions.push(Number(value));
					else probe.sources.push(String(value));
					descriptor.set!.call(this, value);
				}
			});
		}
	});

	const counter = { fetches: 0 };
	await seedQueue(page, { current: ITEMS }, ['row-mike', 'row-zulu', 'row-alpha', 'row-bravo'], counter);
	await advanceTo(page, 'row-zulu', 1);
	counter.fetches = 0;

	await removeButton(page, 'row-zulu').click();
	const undo = await expectUndoOffered(page, 'row-zulu');
	await undo.click();

	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu', 'row-alpha', 'row-bravo']);
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');
	expect(counter.fetches).toBe(0);
	expect(
		await page.evaluate(
			() => (window as unknown as { undoProbe: unknown }).undoProbe
		)
	).toEqual({ media: [], positions: [], sources: [] });
	await expect(page.locator('audio')).toHaveCount(0);
});

test('UNDO-B20 narrow viewport: the undo offer stays visible and restores on tap-sized targets', async ({
	page
}) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await seedQueue(page);

	await removeButton(page, 'row-alpha').click();
	const undo = await expectUndoOffered(page, 'row-alpha');
	const bounds = await undo.boundingBox();
	expect(bounds).not.toBeNull();
	expect(bounds!.x).toBeGreaterThanOrEqual(0);
	expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(390);

	await undo.click();
	await expect.poll(() => queueOrder(page)).toEqual([
		'row-mike',
		'row-zulu',
		'row-alpha',
		'row-bravo'
	]);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
});
