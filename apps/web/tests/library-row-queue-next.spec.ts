import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Acceptance: queue a browsed library track next without interrupting the
 * current selection.
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build lands the
 * library-row Queue next wiring on top of this branch and must make these
 * tests pass WITHOUT editing this file; a wrong test comes back to the
 * acceptance lane instead.
 *
 * Harness note (same as the neighbouring library specs): `/library` is
 * client-fetched (`ssr = false`), so the click-driven contract is pinned here
 * in the offline Playwright suite with a route-mocked `/api/library`. Every
 * test guards its first new-control interaction with an explicit visibility
 * assertion, so a run against a build without the controls fails as an
 * assertion (missing control), never as a click error. Fully offline and
 * deterministic: no backend, no provider, no spend.
 *
 * Hooks (new component-owned literals, same rule as `queue-item` /
 * `now-playing`, which live in a component):
 * - One `button[type=button]` per loaded library row, scoped inside
 *   `[data-testid="library-item"][data-id="<id>"]`, with the exact accessible
 *   name `Queue next <display title>` and visible text matching /Queue next/.
 *   Native keyboard/touch behavior; activation never bubbles into the row
 *   append, star or copy-link handlers.
 * - `queue-next-result` (`data-testid`, `role="status"`) reporting the last
 *   activation only, unmounted when there is no feedback to show (same shape
 *   as `append-result`). Exact one-shot texts:
 *   inserted `Queued <title> next.`, moved `Moved <title> to next.`,
 *   already-next `<title> is already next.`, current
 *   `<title> is already the current track.`, no-longer-loaded
 *   `That track is no longer in the library.` Never a playing claim.
 *
 * Contract under test (paired build):
 * - One stable-ID transform: a missing track is inserted immediately after
 *   current; an already-queued noncurrent track moves there without
 *   duplication; the current track itself is a no-op. Current ID/source and
 *   surviving relative order are preserved. An empty queue gains one
 *   selected metadata-only row, never audible playback. Repeated activation
 *   is idempotent.
 * - Inserted rows use the existing metadata-only queued-track policy (display
 *   fields only, no stream URL); moved rows keep their queued metadata and
 *   source untouched. Never play/load/seek, fetch/renew a stream, mutate
 *   favorites or the pending selection, auto-advance, or promise playback.
 * - The row resolves by current loaded stable ID at activation; refresh and
 *   auth teardown reconcile stale feedback; a refresh that removes the row
 *   prunes it (existing per-account rule) and clears its feedback.
 * - Focus stays on the initiating surviving action; no scroll or focus theft.
 * - Existing row append, star, copy link, find/views and queue-panel Play
 *   next keep working unchanged.
 *
 * Case IDs (stable): QN-B-00 control, QN-B-01 render, QN-B-02 empty queue,
 * QN-B-03 insert after current, QN-B-04 move from after, QN-B-05 move from
 * before, QN-B-06 current no-op, QN-B-07 already-next no-op, QN-B-08 repeat
 * idempotent, QN-B-09 no bubble, QN-B-10 keyboard and focus, QN-B-11 touch,
 * QN-B-12 filtered view, QN-B-13 favorites view, QN-B-14 refresh
 * reconciliation, QN-B-15 no-longer-loaded, QN-B-16 auth reset, QN-B-17 zero
 * network mutation, QN-B-18 narrow both-theme, QN-B-19 insert uses loaded
 * metadata, QN-B-20 move retains queued metadata, QN-B-21 existing actions
 * preserved, QN-B-22 names and badges.
 *
 * Out of scope (not pinned): queue autoplay/auto-next, stream activation or
 * grant renewal, bulk insertion, radio, repeat, cross-route playback,
 * backend playlists, and replacing the queue-panel Play next.
 */

// Rendered order is API order; ids are deliberately not in sorted order.
// Titles/artists avoid "star", "favorite", "select", "append", "clear",
// "queue", "next" and "play" so name-filtered locators below can only match
// their intended control.
const ITEMS = [
	{ id: 'qn-ember', title: 'Ember Song', artist: 'Harbor Lights' },
	{ id: 'qn-cinder', title: 'Cinder Song' },
	{ id: 'qn-flint', title: 'Flint Song', artist: 'Harbor Lights' },
	{ id: 'qn-ash', title: 'Ash Song', artist: 'Stone Choir' }
];

const NEXT_ACCOUNT = [{ id: 'qn-willow', title: 'Willow Song', artist: 'Stone Choir' }];

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

function rowAppendButton(page: Page, id: string): Locator {
	return row(page, id).locator('button.nt-library-row');
}

function queueNextButton(page: Page, id: string, title: string): Locator {
	return row(page, id).getByRole('button', { name: `Queue next ${title}`, exact: true });
}

function starButton(page: Page, id: string): Locator {
	return row(page, id).getByRole('button', { name: /star|favorite/i });
}

function feedback(page: Page): Locator {
	return page.getByTestId('queue-next-result');
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

async function ariaCurrent(locator: Locator): Promise<string | null> {
	return locator.evaluate((element) => element.getAttribute('aria-current'));
}

/** Append every id in order through the existing row-append action. */
async function appendRows(page: Page, ids: string[]): Promise<void> {
	for (const id of ids) {
		await rowAppendButton(page, id).click();
	}
	await expect(page.getByTestId('queue-item')).toHaveCount(ids.length);
}

async function fireAuthTransition(page: Page): Promise<void> {
	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate((key) => {
		window.localStorage.setItem(key, 'test-queue-next-auth-transition');
		window.localStorage.removeItem(key);
	}, 'nntune.library.auth-transition.v1');
	await otherTab.close();
}

async function expectNoAudiblePlayback(page: Page): Promise<void> {
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
	await expect(page.getByTestId('library-now-playing')).toHaveCount(0);
}

test('[QN-B-00] control: the library renders fixture rows in API order', async ({ page }) => {
	await seedLibrary(page);

	const rendered = await page
		.getByTestId('library-item')
		.evaluateAll((items) => items.map((item) => item.getAttribute('data-id')));
	expect(rendered).toEqual(['qn-ember', 'qn-cinder', 'qn-flint', 'qn-ash']);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
});

test('[QN-B-01] each loaded row carries a clearly labelled Queue next action', async ({
	page
}) => {
	await seedLibrary(page);

	for (const item of ITEMS) {
		const button = queueNextButton(page, item.id, item.title);
		await expect(button, `Queue next missing on row ${item.id}`).toBeVisible();
		await expect(button).toHaveAttribute('type', 'button');
		await expect(button).toHaveAccessibleName(`Queue next ${item.title}`);
		await expect(button, `Queue next on ${item.id} must be visibly labelled`).toContainText(
			/Queue next/
		);
		await expect(button).toBeEnabled();
		// Exactly one Queue next action per row: no duplicate controls.
		await expect(row(page, item.id).getByRole('button', { name: /Queue next/ })).toHaveCount(1);
	}
	// Nothing is queued or announced by rendering alone.
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(feedback(page)).toHaveCount(0);
});

test('[QN-B-02] an empty queue gains one selected metadata-only row, never playback', async ({
	page
}) => {
	await seedLibrary(page);

	const button = queueNextButton(page, 'qn-cinder', 'Cinder Song');
	await expect(button, 'Queue next missing on row qn-cinder').toBeVisible();
	await button.click();

	expect(await queueOrder(page)).toEqual(['qn-cinder']);
	await expect(queueItem(page, 'qn-cinder')).toContainText('Cinder Song');
	await expect(await ariaCurrent(queueItem(page, 'qn-cinder'))).toBe('true');
	await expect(page.getByTestId('now-playing')).toContainText('Cinder Song');
	await expect(page.getByTestId('queue-position')).toContainText('Track 1 of 1');
	await expect(feedback(page)).toHaveText('Queued Cinder Song next.');
	await expectNoAudiblePlayback(page);
});

test('[QN-B-03] a missing track is inserted immediately after current', async ({ page }) => {
	await seedLibrary(page);
	await appendRows(page, ['qn-ember', 'qn-flint', 'qn-ash']);

	const button = queueNextButton(page, 'qn-cinder', 'Cinder Song');
	await expect(button, 'Queue next missing on row qn-cinder').toBeVisible();
	await button.click();

	// Inserted after current (ember), survivors keep relative order, no dupes.
	expect(await queueOrder(page)).toEqual(['qn-ember', 'qn-cinder', 'qn-flint', 'qn-ash']);
	expect(new Set(await queueOrder(page)).size).toBe(4);
	await expect(await ariaCurrent(queueItem(page, 'qn-ember'))).toBe('true');
	await expect(page.getByTestId('now-playing')).toContainText('Ember Song');
	await expect(feedback(page)).toHaveText('Queued Cinder Song next.');
	await expectNoAudiblePlayback(page);
});

test('[QN-B-04] an already-queued later track moves next without duplication', async ({
	page
}) => {
	await seedLibrary(page);
	await appendRows(page, ['qn-ember', 'qn-flint', 'qn-ash']);

	const button = queueNextButton(page, 'qn-ash', 'Ash Song');
	await expect(button, 'Queue next missing on row qn-ash').toBeVisible();
	await button.click();

	expect(await queueOrder(page)).toEqual(['qn-ember', 'qn-ash', 'qn-flint']);
	expect(new Set(await queueOrder(page)).size).toBe(3);
	await expect(await ariaCurrent(queueItem(page, 'qn-ember'))).toBe('true');
	await expect(page.getByTestId('now-playing')).toContainText('Ember Song');
	await expect(feedback(page)).toHaveText('Moved Ash Song to next.');
	await expectNoAudiblePlayback(page);
});

test('[QN-B-05] an already-queued earlier track moves next and current is stable', async ({
	page
}) => {
	await seedLibrary(page);
	await appendRows(page, ['qn-ember', 'qn-flint', 'qn-ash']);
	// Move current to the middle through the existing queue-panel selection.
	await queueItem(page, 'qn-flint').getByRole('button', { name: 'Select Flint Song' }).click();
	await expect(await ariaCurrent(queueItem(page, 'qn-flint'))).toBe('true');

	const button = queueNextButton(page, 'qn-ember', 'Ember Song');
	await expect(button, 'Queue next missing on row qn-ember').toBeVisible();
	await button.click();

	expect(await queueOrder(page)).toEqual(['qn-flint', 'qn-ember', 'qn-ash']);
	expect(new Set(await queueOrder(page)).size).toBe(3);
	await expect(await ariaCurrent(queueItem(page, 'qn-flint'))).toBe('true');
	await expect(page.getByTestId('now-playing')).toContainText('Flint Song');
	await expect(feedback(page)).toHaveText('Moved Ember Song to next.');
	await expectNoAudiblePlayback(page);
});

test('[QN-B-06] queueing the current track itself is a truthful no-op', async ({ page }) => {
	await seedLibrary(page);
	await appendRows(page, ['qn-ember', 'qn-flint']);

	const button = queueNextButton(page, 'qn-ember', 'Ember Song');
	await expect(button, 'Queue next missing on row qn-ember').toBeVisible();
	await button.click();

	expect(await queueOrder(page)).toEqual(['qn-ember', 'qn-flint']);
	await expect(await ariaCurrent(queueItem(page, 'qn-ember'))).toBe('true');
	await expect(feedback(page)).toHaveText('Ember Song is already the current track.');
	await expectNoAudiblePlayback(page);
});

test('[QN-B-07] queueing the already-next track is a truthful no-op', async ({ page }) => {
	await seedLibrary(page);
	await appendRows(page, ['qn-ember', 'qn-flint', 'qn-ash']);

	const button = queueNextButton(page, 'qn-flint', 'Flint Song');
	await expect(button, 'Queue next missing on row qn-flint').toBeVisible();
	await button.click();

	expect(await queueOrder(page)).toEqual(['qn-ember', 'qn-flint', 'qn-ash']);
	await expect(await ariaCurrent(queueItem(page, 'qn-ember'))).toBe('true');
	await expect(feedback(page)).toHaveText('Flint Song is already next.');
	await expectNoAudiblePlayback(page);
});

test('[QN-B-08] repeated activation is idempotent with one badge and one message', async ({
	page
}) => {
	await seedLibrary(page);

	const button = queueNextButton(page, 'qn-ash', 'Ash Song');
	await expect(button, 'Queue next missing on row qn-ash').toBeVisible();
	await button.click();
	await button.click();
	await button.click();

	expect(await queueOrder(page)).toEqual(['qn-ash']);
	// One row badge per queued row, one feedback message per activation run.
	await expect(row(page, 'qn-ash').getByTestId('library-queued-badge')).toHaveCount(1);
	await expect(page.getByTestId('library-queued-badge')).toHaveCount(1);
	await expect(feedback(page)).toHaveCount(1);
	await expectNoAudiblePlayback(page);
});

test('[QN-B-09] activation never touches selection, star, copy or find state', async ({
	page
}) => {
	await seedLibrary(page);

	const button = queueNextButton(page, 'qn-flint', 'Flint Song');
	await expect(button, 'Queue next missing on row qn-flint').toBeVisible();
	await button.click();

	// Non-vacuous gate: the activation under observation queued its track.
	expect(await queueOrder(page)).toEqual(['qn-flint']);

	await expect(page.getByTestId('selection-count')).toContainText('0 selected');
	for (const item of ITEMS) {
		await expect(row(page, item.id).getByRole('checkbox')).not.toBeChecked();
		await expect(starButton(page, item.id)).toHaveAttribute('aria-pressed', 'false');
	}
	await expect(page.getByTestId('favorite-item')).toHaveCount(0);
	await expect(page.getByTestId('favorites-empty')).toBeVisible();
	await expect(page.getByTestId('library-share-status')).toBeEmpty();
	await expect(page.getByTestId('library-find-count')).toContainText('Showing 4 of 4 tracks');
	await expect(page.getByLabel('Find in loaded tracks')).toBeEmpty();
});

test('[QN-B-10] keyboard activation keeps focus and scroll position', async ({ page }) => {
	await seedLibrary(page);
	await appendRows(page, ['qn-ember', 'qn-flint']);

	const button = queueNextButton(page, 'qn-ash', 'Ash Song');
	await expect(button, 'Queue next missing on row qn-ash').toBeVisible();
	await button.focus();
	await expect(button).toBeFocused();

	const before = await page.evaluate(() => window.scrollY);
	await page.keyboard.press('Enter');

	expect(await queueOrder(page)).toEqual(['qn-ember', 'qn-ash', 'qn-flint']);
	await expect(button, 'focus must stay on the initiating action').toBeFocused();
	expect(await page.evaluate(() => window.scrollY)).toBe(before);
	await expect(feedback(page)).toHaveText('Queued Ash Song next.');

	// Space activates natively too and still keeps focus.
	await page.keyboard.press('Space');
	expect(await queueOrder(page)).toEqual(['qn-ember', 'qn-ash', 'qn-flint']);
	await expect(button).toBeFocused();
	await expect(feedback(page)).toHaveText('Ash Song is already next.');
});

test.describe('touch activation', () => {
	test.use({ hasTouch: true });

	test('[QN-B-11] touch activation queues next without moving focus away', async ({ page }) => {
		await seedLibrary(page);
		await appendRows(page, ['qn-ember', 'qn-flint']);

		const button = queueNextButton(page, 'qn-cinder', 'Cinder Song');
		await expect(button, 'Queue next missing on row qn-cinder').toBeVisible();
		await button.tap();

		expect(await queueOrder(page)).toEqual(['qn-ember', 'qn-cinder', 'qn-flint']);
		await expect(await ariaCurrent(queueItem(page, 'qn-ember'))).toBe('true');
		await expect(feedback(page)).toHaveText('Queued Cinder Song next.');
		await expectNoAudiblePlayback(page);
	});
});

test('[QN-B-12] a filtered row queues next and the find state survives', async ({ page }) => {
	await seedLibrary(page);
	await appendRows(page, ['qn-ember', 'qn-flint']);

	await page.getByLabel('Find in loaded tracks').fill('ash');
	await expect(page.getByTestId('library-item')).toHaveCount(1);

	const button = queueNextButton(page, 'qn-ash', 'Ash Song');
	await expect(button, 'Queue next missing on filtered row qn-ash').toBeVisible();
	await button.click();

	expect(await queueOrder(page)).toEqual(['qn-ember', 'qn-ash', 'qn-flint']);
	await expect(feedback(page)).toHaveText('Queued Ash Song next.');
	await expect(page.getByLabel('Find in loaded tracks')).toHaveValue('ash');
	await expect(page.getByTestId('library-item')).toHaveCount(1);
	await expectNoAudiblePlayback(page);
});

test('[QN-B-13] favorites-only rows queue next and favorites stay untouched', async ({
	page
}) => {
	await seedLibrary(page);

	await starButton(page, 'qn-ember').click();
	await starButton(page, 'qn-ash').click();
	await expect(page.getByTestId('favorite-item')).toHaveCount(2);

	await page.locator('#library-view').selectOption('favorites');
	await expect(page.getByTestId('library-item')).toHaveCount(2);

	const button = queueNextButton(page, 'qn-ash', 'Ash Song');
	await expect(button, 'Queue next missing on favorites-view row qn-ash').toBeVisible();
	await button.click();

	expect(await queueOrder(page)).toEqual(['qn-ash']);
	await expect(feedback(page)).toHaveText('Queued Ash Song next.');
	// Favorites are unchanged: still starred, shelf intact.
	await expect(starButton(page, 'qn-ember')).toHaveAttribute('aria-pressed', 'true');
	await expect(starButton(page, 'qn-ash')).toHaveAttribute('aria-pressed', 'true');
	await expect(page.getByTestId('favorite-item')).toHaveCount(2);
	await expectNoAudiblePlayback(page);
});

test('[QN-B-14] a refresh that removes the row prunes it and clears stale feedback', async ({
	page
}) => {
	const payload: { current: unknown } = { current: ITEMS };
	await seedLibrary(page, payload);

	const button = queueNextButton(page, 'qn-flint', 'Flint Song');
	await expect(button, 'Queue next missing on row qn-flint').toBeVisible();
	await button.click();
	await expect(feedback(page)).toHaveText('Queued Flint Song next.');

	payload.current = ITEMS.filter((item) => item.id !== 'qn-flint');
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(row(page, 'qn-flint')).toHaveCount(0);

	// Existing per-account prune drops the missing id; the stale claim is gone.
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(feedback(page)).toHaveCount(0);
	await expect(page.getByRole('alert')).toHaveCount(0);
});

test('[QN-B-15] activating for a no-longer-loaded row reports truthfully', async ({ page }) => {
	const payload: { current: unknown } = { current: ITEMS };
	await seedLibrary(page, payload);
	await appendRows(page, ['qn-ember', 'qn-flint']);

	const button = queueNextButton(page, 'qn-flint', 'Flint Song');
	await expect(button, 'Queue next missing on row qn-flint').toBeVisible();
	// Hold the node across a refresh that removes its row, then fire the
	// activation the refresh raced: the handler must resolve the missing id.
	const handle = await button.elementHandle();
	expect(handle, 'Queue next handle missing for qn-flint').not.toBeNull();

	payload.current = ITEMS.filter((item) => item.id !== 'qn-flint');
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(row(page, 'qn-flint')).toHaveCount(0);

	await handle!.evaluate((node) => (node as HTMLElement).click());
	await expect(feedback(page)).toHaveText('That track is no longer in the library.');
	// The surviving queue is untouched: still ember, still current.
	expect(await queueOrder(page)).toEqual(['qn-ember']);
	await expect(await ariaCurrent(queueItem(page, 'qn-ember'))).toBe('true');
	await expectNoAudiblePlayback(page);
});

test('[QN-B-16] an auth reset reconciles the queue and clears stale feedback', async ({
	page
}) => {
	const payload: { current: unknown } = { current: ITEMS };
	await seedLibrary(page, payload);

	const button = queueNextButton(page, 'qn-ember', 'Ember Song');
	await expect(button, 'Queue next missing on row qn-ember').toBeVisible();
	await button.click();
	await expect(feedback(page)).toHaveText('Queued Ember Song next.');

	await fireAuthTransition(page);
	await expect(page.getByRole('alert')).toContainText(/session changed/i);

	payload.current = NEXT_ACCOUNT;
	// Stale label note: the boundary button reads Retry (see LF-10 pin).
	await page.getByRole('button', { name: 'Retry' }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(NEXT_ACCOUNT.length);
	await expect(page.getByTestId('library-list')).toContainText('Willow Song');
	// Existing per-account prune drops ember; no stale claim survives.
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(feedback(page)).toHaveCount(0);
});

test('[QN-B-17] queue-next flows fetch, stream and mutate nothing over the network', async ({
	page
}) => {
	const requests: { method: string; url: string }[] = [];
	page.on('request', (request) => requests.push({ method: request.method(), url: request.url() }));
	await seedLibrary(page);
	await appendRows(page, ['qn-ember', 'qn-flint', 'qn-ash']);

	for (const [id, title] of [
		['qn-cinder', 'Cinder Song'],
		['qn-ash', 'Ash Song'],
		['qn-ember', 'Ember Song']
	] as const) {
		const button = queueNextButton(page, id, title);
		await expect(button, `Queue next missing on row ${id}`).toBeVisible();
		await button.click();
	}

	// Non-vacuous gate: insert, move and current no-op all ran under watch.
	expect(await queueOrder(page)).toEqual(['qn-ember', 'qn-ash', 'qn-cinder', 'qn-flint']);
	await expect(feedback(page)).toHaveText('Ember Song is already the current track.');

	const libraryGets = requests.filter(
		(request) => request.method === 'GET' && request.url.includes('/api/library')
	);
	expect(libraryGets, 'queue-next flows must not refetch the catalog').toHaveLength(1);
	expect(
		requests.filter((request) => request.method !== 'GET'),
		'queue-next flows must not mutate over the network'
	).toEqual([]);
	expect(
		requests.filter((request) => /demo\/audio|stream|media/i.test(request.url)),
		'queue-next flows must not touch media endpoints'
	).toEqual([]);
	await expectNoAudiblePlayback(page);
});

test('[QN-B-18] narrow-screen light and dark keep the action visible and operable', async ({
	page
}) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await seedLibrary(page);
	await appendRows(page, ['qn-ember', 'qn-flint']);

	for (const scheme of ['light', 'dark'] as const) {
		await page.emulateMedia({ colorScheme: scheme });

		const button = queueNextButton(page, 'qn-cinder', 'Cinder Song');
		await expect(button, `Queue next missing on row qn-cinder (${scheme})`).toBeVisible();
		await button.click();

		expect(await queueOrder(page)).toEqual(['qn-ember', 'qn-cinder', 'qn-flint']);
		await expect(feedback(page), `feedback missing (${scheme})`).toBeVisible();
	}
	await expectNoAudiblePlayback(page);
});

test('[QN-B-19] an inserted row carries current loaded metadata, never a stream', async ({
	page
}) => {
	const payload: { current: unknown } = { current: ITEMS };
	await seedLibrary(page, payload);

	// Rename the track in the catalog before queueing: the insert resolves
	// the row by stable ID at activation and takes the loaded metadata.
	payload.current = ITEMS.map((item) =>
		item.id === 'qn-flint' ? { ...item, title: 'Flint Remix' } : item
	);
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(row(page, 'qn-flint')).toContainText('Flint Remix');

	const button = queueNextButton(page, 'qn-flint', 'Flint Remix');
	await expect(button, 'Queue next missing on renamed row qn-flint').toBeVisible();
	await button.click();

	expect(await queueOrder(page)).toEqual(['qn-flint']);
	await expect(queueItem(page, 'qn-flint')).toContainText('Flint Remix');
	await expect(feedback(page)).toHaveText('Queued Flint Remix next.');
	await expectNoAudiblePlayback(page);
});

test('[QN-B-20] a moved row retains its queued metadata and source untouched', async ({
	page
}) => {
	const payload: { current: unknown } = { current: ITEMS };
	await seedLibrary(page, payload);
	await appendRows(page, ['qn-ember', 'qn-flint', 'qn-cinder']);

	// Rename the queued track in the catalog: moving it next must keep the
	// metadata it was queued with, not refresh it from the new snapshot.
	payload.current = ITEMS.map((item) =>
		item.id === 'qn-cinder' ? { ...item, title: 'Cinder Remix' } : item
	);
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(row(page, 'qn-cinder')).toContainText('Cinder Remix');

	const button = queueNextButton(page, 'qn-cinder', 'Cinder Remix');
	await expect(button, 'Queue next missing on renamed row qn-cinder').toBeVisible();
	await button.click();

	expect(await queueOrder(page)).toEqual(['qn-ember', 'qn-cinder', 'qn-flint']);
	await expect(feedback(page)).toHaveText('Moved Cinder Remix to next.');
	await expect(queueItem(page, 'qn-cinder')).toContainText('Cinder Song');
	await expect(queueItem(page, 'qn-cinder')).not.toContainText('Cinder Remix');
	await expect(await ariaCurrent(queueItem(page, 'qn-ember'))).toBe('true');
	await expectNoAudiblePlayback(page);
});

test('[QN-B-21] row append, star, copy and panel Play next keep working', async ({ page }) => {
	await seedLibrary(page);

	const button = queueNextButton(page, 'qn-ember', 'Ember Song');
	await expect(button, 'Queue next missing on row qn-ember').toBeVisible();

	// Existing immediate row append still appends and badges.
	await rowAppendButton(page, 'qn-cinder').click();
	await rowAppendButton(page, 'qn-flint').click();
	expect(await queueOrder(page)).toEqual(['qn-cinder', 'qn-flint']);
	await expect(row(page, 'qn-cinder').getByTestId('library-queued-badge')).toContainText(
		'Queued'
	);

	// Queue next composes with prior appends: inserted after current.
	await button.click();
	expect(await queueOrder(page)).toEqual(['qn-cinder', 'qn-ember', 'qn-flint']);

	// Existing star and copy-link actions are intact on every row.
	for (const item of ITEMS) {
		await expect(starButton(page, item.id)).toBeVisible();
		await expect(
			row(page, item.id).getByTestId('library-copy-link')
		).toHaveAccessibleName(`Copy link ${item.title}`);
	}
	await starButton(page, 'qn-ash').click();
	await expect(starButton(page, 'qn-ash')).toHaveAttribute('aria-pressed', 'true');

	// Existing queue-panel Play next still moves per its own semantics.
	await queueItem(page, 'qn-flint').getByRole('button', { name: 'Play Flint Song next' }).click();
	expect(await queueOrder(page)).toEqual(['qn-cinder', 'qn-flint', 'qn-ember']);
	await expect(await ariaCurrent(queueItem(page, 'qn-cinder'))).toBe('true');
});

test('[QN-B-22] feedback never claims playback and badges stay singular', async ({ page }) => {
	await seedLibrary(page);
	await appendRows(page, ['qn-ember', 'qn-flint']);

	for (const [id, title] of [
		['qn-cinder', 'Cinder Song'],
		['qn-flint', 'Flint Song'],
		['qn-ember', 'Ember Song']
	] as const) {
		const button = queueNextButton(page, id, title);
		await expect(button, `Queue next missing on row ${id}`).toBeVisible();
		await button.click();

		const text = (await feedback(page).textContent()) ?? '';
		expect(text, 'feedback must never claim playback').not.toMatch(/playing/i);
		expect(text, 'feedback names its track').toContain(title);
	}

	expect(await queueOrder(page)).toEqual(['qn-ember', 'qn-flint', 'qn-cinder']);
	// Exactly one Queued badge per queued row: none duplicated, none invented.
	await expect(page.getByTestId('library-queued-badge')).toHaveCount(3);
	for (const id of ['qn-ember', 'qn-cinder', 'qn-flint']) {
		await expect(row(page, id).getByTestId('library-queued-badge')).toHaveCount(1);
	}
	await expect(row(page, 'qn-ash').getByTestId('library-queued-badge')).toHaveCount(0);
	await expectNoAudiblePlayback(page);
});
