import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Acceptance: announce browse-list outcomes to assistive tech.
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build lands polite
 * browse-outcome announcements on top of this branch and must make these
 * tests pass WITHOUT editing this file; a wrong test comes back to the
 * acceptance lane instead. There is no pure half: live-region exposure,
 * coalescing and announcement text are DOM behavior, pinned here only.
 *
 * Harness note (same as the neighbouring library specs): `/library` is
 * client-fetched (`ssr = false`), so the click/keyboard contract is pinned
 * here in the offline Playwright suite with a route-mocked `/api/library`.
 * Every behavior test guards its first interaction with an explicit
 * option-count assertion, so a run against a build without the control
 * fails as an assertion (missing control), never as a click timeout.
 * Fully offline and deterministic: no backend, no provider, no spend.
 *
 * Locked surface (the build announces through it; wording is the contract):
 * - The existing `library-find-count` (`Showing <d> of <n> tracks`) and
 *   `selection-count` (`<s> selected`) outcomes are exposed through polite
 *   atomic live regions (`role=status`, atomic), following the existing
 *   queue/favorites/append/share sibling pattern. Typing coalesces: no
 *   assertive region, no per-keystroke queue beyond the latest outcome, no
 *   announcement when the value is unchanged.
 * - Order changes announce the new order once (`List ordered: {label}`,
 *   where {label} is the selected option's label) through one shared
 *   polite status node following the versioned re-announce pattern: a new
 *   outcome replaces the previous text in that node, never alongside it.
 * - View changes announce the new scope once (`Showing {All tracks|
 *   Favorites only|Selected only}`) through the same shared node,
 *   including the `Show all tracks` recovery buttons, which change the
 *   view without going through the select.
 * - Clearing a non-empty selection announces `Selection cleared` once and
 *   clearing a non-empty find announces `Search cleared` once, through the
 *   same shared node. Clearing an already-empty selection/find stays
 *   silent, as does reselecting the current order/view.
 * - No visual-layout change beyond what AT needs: the shared node is
 *   AT-only (visually hidden) and mounted controls/rows never shift.
 *   Reduced-motion safe. Announcements never leak account details,
 *   tokens, or raw metadata: each outcome is one short label-only string.
 * - Existing zero-assertive suite rules still hold, scoped per the
 *   suite's announcer rules (SvelteKit's framework-owned
 *   `#svelte-announcer`, aria-live assertive outside <main>, is required
 *   for route announcements and must not count as build-added chatter).
 *
 * Fixture discipline (same as the find specs): visible title/artist text
 * is DIGIT-FREE, so bare word-boundary digits in <main> can only be the
 * matching/loaded counts — and count assertions never run beside a
 * non-empty queue (queue-position text carries digits), so rows are only
 * ever selected via checkboxes, never clicked. Fixture titles/artists
 * also avoid the words "star" and "favorite" so the star-toggle name
 * filter matches only the toggle itself, and avoid the announcement
 * vocabulary (List/Showing/ordered/selected/cleared) so outcome text can
 * only come from the live regions. Queries are chosen so each expectation
 * is exact: 'song' hits three rows, 'tune' hits one row, 'zzz' hits none,
 * 'BETA' matches exactly what 'beta' matches (case folding).
 *
 * Case IDs (stable): BA-B-00 control, BA-B-01 find-count live region,
 * BA-B-02 selection-count live region, BA-B-03 typing coalesces, BA-B-04
 * silence on unchanged values, BA-B-05 order announces once, BA-B-06 view
 * announces once, BA-B-07 clear announces once, BA-B-08 no visual-layout
 * change, BA-B-09 no leaked content, BA-B-10 zero-assertive rules hold,
 * BA-B-11 narrow light/dark with reduced motion.
 *
 * Focused command (repo root, deterministic, offline):
 *   CI=1 PUBLIC_API_BASE_URL='' pnpm --dir apps/web exec playwright test \
 *     tests/library-browse-announcements.spec.ts --workers=1 --retries=0
 */

const ITEMS = [
	{ id: 'browse-alpha', title: 'Alpha Song', artist: 'Harbor Lights' },
	{ id: 'browse-beta', title: 'Beta Song', artist: 'harbor lights' },
	{ id: 'browse-gamma', title: 'Gamma Tune', artist: 'Echo Field' },
	{ id: 'browse-delta', title: 'Delta Song', artist: 'Night Owls' }
];

const API_IDS = ITEMS.map((item) => item.id);

async function mockLibrary(page: Page): Promise<void> {
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(ITEMS)
		})
	);
}

async function openLibrary(page: Page): Promise<void> {
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expectView(page, API_IDS);
}

async function expectView(page: Page, ids: string[]): Promise<void> {
	await expect
		.poll(() =>
			page
				.getByTestId('library-item')
				.evaluateAll((rows) => rows.map((row) => row.getAttribute('data-id')))
		)
		.toEqual(ids);
}

function findField(page: Page): Locator {
	// Same vocabulary as the find specs: type search (role searchbox) or
	// type text (role textbox); either way the accessible name must mention
	// find or search. A "Clear search" button never matches these roles.
	return page
		.getByRole('searchbox', { name: /find|search/i })
		.or(page.getByRole('textbox', { name: /find|search/i }));
}

async function requireFind(page: Page): Promise<Locator> {
	const field = findField(page);
	await expect(field, 'library must expose a labeled find-in-loaded-tracks field').toBeVisible();
	return field;
}

function clearSearch(page: Page): Locator {
	// "Clear queue" must not match: the control has to say what it clears.
	// first() because the build may render the affordance twice (toolbar
	// plus no-match state); either one must reset.
	return page.getByRole('button', { name: 'Clear search', exact: true }).first();
}

function orderSelect(page: Page): Locator {
	return page.getByLabel('Order tracks', { exact: true });
}

function viewSelect(page: Page): Locator {
	return page.getByLabel('Show', { exact: true });
}

/** Polite status nodes in <main> currently carrying the given outcome text. */
function outcome(page: Page, text: string): Locator {
	return page.locator('main [role="status"], main [aria-live="polite"]', { hasText: text });
}

/**
 * The shared outcome node must be polite (role=status, which implies
 * atomic, or an explicit polite live region) and never assertive. Fails on
 * the pre-implementation base, where the outcome text exists nowhere.
 */
async function expectOutcomeOnce(page: Page, text: string): Promise<Locator> {
	const node = outcome(page, text);
	await expect(node, `expected one polite outcome announcing "${text}"`).toHaveCount(1);
	const role = await node.first().getAttribute('role');
	const live = await node.first().getAttribute('aria-live');
	expect(
		role === 'status' || live === 'polite',
		`outcome "${text}" must live in a polite status node (role=${role} aria-live=${live})`
	).toBe(true);
	expect(live, `outcome "${text}" must never be assertive`).not.toBe('assertive');
	return node;
}

/**
 * A count outcome is exposed to assistive tech only through a polite
 * atomic live region: role=status (atomic by implication) or an explicit
 * aria-live=polite with aria-atomic=true. Fails on the
 * pre-implementation base, where both counts are plain <p>.
 */
async function expectPoliteAtomic(node: Locator, name: string): Promise<void> {
	const role = await node.getAttribute('role');
	const live = await node.getAttribute('aria-live');
	const atomic = await node.getAttribute('aria-atomic');
	expect(
		role === 'status' || live === 'polite',
		`${name} must be a polite live region (role=${role} aria-live=${live})`
	).toBe(true);
	expect(live, `${name} must never be assertive`).not.toBe('assertive');
	expect(
		role === 'status' || atomic === 'true',
		`${name} must be atomic (role=${role} aria-atomic=${atomic})`
	).toBe(true);
}

/**
 * Count DOM mutations on one node while an action runs. Zero means the
 * live region had nothing new to say: no announcement when the value is
 * unchanged. The observer is always disconnected before returning.
 */
async function countMutationsDuring(
	page: Page,
	target: Locator,
	action: () => Promise<unknown>
): Promise<number> {
	await target.evaluate((node) => {
		const win = node.ownerDocument.defaultView as Window & {
			__baMutations?: number;
			__baObserver?: MutationObserver;
		};
		win.__baMutations = 0;
		win.__baObserver?.disconnect();
		const observer = new MutationObserver(() => {
			win.__baMutations = (win.__baMutations ?? 0) + 1;
		});
		observer.observe(node, {
			childList: true,
			characterData: true,
			subtree: true,
			attributes: true
		});
		win.__baObserver = observer;
	});
	await action();
	// Mutations dispatch on the microtask queue; one short settle window
	// keeps the zero assertion honest without polling for absence.
	await page.waitForTimeout(200);
	return target.evaluate((node) => {
		const win = node.ownerDocument.defaultView as Window & {
			__baMutations?: number;
			__baObserver?: MutationObserver;
		};
		win.__baObserver?.disconnect();
		win.__baObserver = undefined;
		return win.__baMutations ?? 0;
	});
}

async function expectZeroAssertive(page: Page, why: string): Promise<void> {
	// Suite rule, both accepted scopings: inside <main> (which excludes the
	// framework-owned #svelte-announcer) and page-wide minus that announcer.
	await expect(
		page.locator('main').locator('[aria-live="assertive"]'),
		`${why}: no assertive live region inside <main>`
	).toHaveCount(0);
	await expect(
		page.locator('[aria-live="assertive"]:not(#svelte-announcer)'),
		`${why}: no build-added assertive live region page-wide`
	).toHaveCount(0);
	await expect(
		page.locator('main').locator('[role="alert"]'),
		`${why}: no alert inside <main>`
	).toHaveCount(0);
}

test('BA-B-00 control: find, order, view and selection hooks are present', async ({ page }) => {
	await mockLibrary(page);
	await openLibrary(page);
	await requireFind(page);
	const order = orderSelect(page);
	await expect(order, 'order control must exist').toBeVisible();
	await expect(order.locator('option')).toHaveCount(5);
	const view = viewSelect(page);
	await expect(view, 'view control must exist').toBeVisible();
	await expect(view.locator('option')).toHaveCount(3);
	await expect(page.getByTestId('library-find-count')).toContainText('Showing 4 of 4 tracks');
	await expect(page.getByTestId('selection-count')).toHaveText('0 selected');
	await expect(
		page.getByRole('button', { name: 'Clear selection', exact: true }),
		'clear-selection control must exist'
	).toBeVisible();
});

test('BA-B-01 find-count is exposed through a polite atomic live region', async ({ page }) => {
	await mockLibrary(page);
	await openLibrary(page);
	const field = await requireFind(page);
	await field.fill('song');
	await expectView(page, ['browse-alpha', 'browse-beta', 'browse-delta']);
	const count = page.getByTestId('library-find-count');
	await expect(count).toContainText('Showing 3 of 4 tracks');
	await expectPoliteAtomic(count, 'library-find-count');
});

test('BA-B-02 selection-count is exposed through a polite atomic live region', async ({ page }) => {
	await mockLibrary(page);
	await openLibrary(page);
	await page.getByRole('checkbox', { name: 'Select Alpha Song', exact: true }).check();
	await page.getByRole('checkbox', { name: 'Select Gamma Tune', exact: true }).check();
	const count = page.getByTestId('selection-count');
	await expect(count).toHaveText('2 selected');
	await expectPoliteAtomic(count, 'selection-count');
});

test('BA-B-03 typing coalesces: latest outcome only, never assertive', async ({ page }) => {
	await mockLibrary(page);
	await openLibrary(page);
	const field = await requireFind(page);
	// Keystroke-by-keystroke typing: intermediate counts must never queue
	// beyond the latest outcome, and filtering must not add assertive
	// chatter (the LF-08 rule, re-pinned for the new live region).
	await field.click();
	await page.keyboard.type('delta', { delay: 30 });
	await expectView(page, ['browse-delta']);
	const count = page.getByTestId('library-find-count');
	await expect(count).toHaveCount(1);
	await expect(count).toContainText('Showing 1 of 4 track');
	await expectPoliteAtomic(count, 'library-find-count');
	await expectZeroAssertive(page, 'typing in find');
});

test('BA-B-04 silence on unchanged values: no announcement without a new outcome', async ({
	page
}) => {
	await mockLibrary(page);
	await openLibrary(page);
	const order = orderSelect(page);
	await expect(order.locator('option')).toHaveCount(5);
	await order.selectOption({ label: 'Title A–Z' });
	const node = await expectOutcomeOnce(page, 'List ordered: Title A–Z');
	// Reselecting the current order changes no value: the shared node must
	// stay untouched (no version bump, no re-announcement of the same text).
	const reselected = await countMutationsDuring(page, node.first(), () =>
		order.selectOption({ label: 'Title A–Z' })
	);
	expect(reselected, 'reselecting the current order must not touch the outcome node').toBe(0);

	// Same-effective query ('BETA' folds to the same match as 'beta'): the
	// count text is unchanged, so the live region must stay untouched.
	const field = await requireFind(page);
	await field.fill('beta');
	await expectView(page, ['browse-beta']);
	const count = page.getByTestId('library-find-count');
	await expect(count).toContainText('Showing 1 of 4 track');
	const refolded = await countMutationsDuring(page, count, () => field.fill('BETA'));
	expect(refolded, 'a query change with the same outcome must not touch the count node').toBe(0);

	// Clearing an already-empty selection has no outcome to announce.
	await expect(page.getByTestId('selection-count')).toHaveText('0 selected');
	await page.getByRole('button', { name: 'Clear selection', exact: true }).click();
	await expect(
		outcome(page, 'Selection cleared'),
		'clearing an empty selection must stay silent'
	).toHaveCount(0);
});

test('BA-B-05 order changes announce the new order once', async ({ page }) => {
	await mockLibrary(page);
	await openLibrary(page);
	const order = orderSelect(page);
	await expect(order.locator('option')).toHaveCount(5);
	await order.selectOption({ label: 'Title A–Z' });
	await expectView(page, ['browse-alpha', 'browse-beta', 'browse-delta', 'browse-gamma']);
	await expectOutcomeOnce(page, 'List ordered: Title A–Z');
	// A second order replaces the first outcome in the shared node: the
	// stale text must be gone, never queued alongside the latest.
	await order.selectOption({ label: 'Artist A–Z' });
	await expectOutcomeOnce(page, 'List ordered: Artist A–Z');
	await expect(
		outcome(page, 'List ordered: Title A–Z'),
		'a replaced order outcome must not linger'
	).toHaveCount(0);
	await expectZeroAssertive(page, 'order changes');
});

test('BA-B-06 view changes announce the new scope once', async ({ page }) => {
	await mockLibrary(page);
	await openLibrary(page);
	const view = viewSelect(page);
	await expect(view.locator('option')).toHaveCount(3);
	await view.selectOption({ label: 'Favorites only' });
	await expectOutcomeOnce(page, 'Showing Favorites only');
	await view.selectOption({ label: 'Selected only' });
	await expectOutcomeOnce(page, 'Showing Selected only');
	await expect(
		outcome(page, 'Showing Favorites only'),
		'a replaced view outcome must not linger'
	).toHaveCount(0);
	// The recovery button changes the view without going through the
	// select; the scope change must still announce once.
	await page.getByRole('button', { name: 'Show all tracks', exact: true }).click();
	await expectView(page, API_IDS);
	await expectOutcomeOnce(page, 'Showing All tracks');
	await expect(
		outcome(page, 'Showing Selected only'),
		'a replaced view outcome must not linger'
	).toHaveCount(0);
	await expectZeroAssertive(page, 'view changes');
});

test('BA-B-07 clearing selection and find announces the cleared outcome once', async ({
	page
}) => {
	await mockLibrary(page);
	await openLibrary(page);
	await page.getByRole('checkbox', { name: 'Select Alpha Song', exact: true }).check();
	await page.getByRole('checkbox', { name: 'Select Delta Song', exact: true }).check();
	await expect(page.getByTestId('selection-count')).toHaveText('2 selected');
	await page.getByRole('button', { name: 'Clear selection', exact: true }).click();
	await expect(page.getByTestId('selection-count')).toHaveText('0 selected');
	await expectOutcomeOnce(page, 'Selection cleared');

	const field = await requireFind(page);
	await field.fill('tune');
	await expectView(page, ['browse-gamma']);
	await clearSearch(page).click();
	await expect(field).toHaveValue('');
	await expectView(page, API_IDS);
	await expectOutcomeOnce(page, 'Search cleared');
	await expect(
		outcome(page, 'Selection cleared'),
		'a replaced clear outcome must not linger'
	).toHaveCount(0);
	await expectZeroAssertive(page, 'clearing selection and find');
});

test('BA-B-08 announcing changes no visual layout beyond AT needs', async ({ page }) => {
	await mockLibrary(page);
	await openLibrary(page);
	const order = orderSelect(page);
	await expect(order.locator('option')).toHaveCount(5);
	const view = viewSelect(page);
	await expect(view.locator('option')).toHaveCount(3);
	// These stay mounted across every browse action below, so any shift is
	// a layout change caused by the announcement nodes, not by the action.
	const findCount = page.getByTestId('library-find-count');
	const list = page.getByTestId('library-list');
	const beforeCount = await findCount.boundingBox();
	const beforeList = await list.boundingBox();
	expect(beforeCount, 'find-count must be laid out before the actions').not.toBeNull();
	expect(beforeList, 'library list must be laid out before the actions').not.toBeNull();

	await order.selectOption({ label: 'Title A–Z' });
	const node = await expectOutcomeOnce(page, 'List ordered: Title A–Z');
	await view.selectOption({ label: 'Favorites only' });
	await expectOutcomeOnce(page, 'Showing Favorites only');
	await view.selectOption({ label: 'All tracks' });
	await page.getByRole('checkbox', { name: 'Select Beta Song', exact: true }).check();
	await page.getByRole('button', { name: 'Clear selection', exact: true }).click();
	await expectOutcomeOnce(page, 'Selection cleared');

	// The shared outcome node is AT-only: visually hidden (sr-only sized
	// or removed from flow), never a new visible status line.
	const box = await node.first().boundingBox();
	const position = await node.first().evaluate((element) => getComputedStyle(element).position);
	expect(
		box !== null && (box.height <= 2 || box.width <= 2 || position === 'absolute' || position === 'fixed'),
		`shared outcome node must be visually hidden (box=${JSON.stringify(box)} position=${position})`
	).toBe(true);

	const afterCount = await findCount.boundingBox();
	const afterList = await list.boundingBox();
	for (const [name, before, after] of [
		['find-count', beforeCount, afterCount],
		['library-list', beforeList, afterList]
	] as const) {
		expect(after, `${name} must still be laid out`).not.toBeNull();
		for (const edge of ['x', 'y', 'width', 'height'] as const) {
			expect(
				Math.abs((after as NonNullable<typeof before>)[edge] - (before as NonNullable<typeof before>)[edge]),
				`${name} ${edge} must not shift while announcing`
			).toBeLessThanOrEqual(1);
		}
	}
});

test('BA-B-09 announcements never leak account, token or raw metadata content', async ({
	page
}) => {
	await mockLibrary(page);
	await openLibrary(page);
	const order = orderSelect(page);
	await expect(order.locator('option')).toHaveCount(5);
	const view = viewSelect(page);
	await expect(view.locator('option')).toHaveCount(3);
	await order.selectOption({ label: 'Title Z–A' });
	await expectOutcomeOnce(page, 'List ordered: Title Z–A');
	await view.selectOption({ label: 'Favorites only' });
	await expectOutcomeOnce(page, 'Showing Favorites only');
	await view.selectOption({ label: 'All tracks' });
	await page.getByRole('checkbox', { name: 'Select Alpha Song', exact: true }).check();
	await page.getByRole('button', { name: 'Clear selection', exact: true }).click();
	await expectOutcomeOnce(page, 'Selection cleared');
	const field = await requireFind(page);
	await field.fill('zzz');
	await expectView(page, []);
	await clearSearch(page).click();
	await expectOutcomeOnce(page, 'Search cleared');

	// Every outcome is one short label-only string: long enough for the
	// longest locked wording, far too short for a token or metadata dump.
	for (const text of [
		'List ordered: Title Z–A',
		'Showing Favorites only',
		'Selection cleared',
		'Search cleared'
	]) {
		const node = outcome(page, text);
		const body = ((await node.first().textContent()) ?? '').trim();
		expect(body.length, `outcome "${text}" must stay concise`).toBeLessThan(120);
	}
	const liveTexts = await page
		.locator('main [role="status"], main [aria-live]')
		.evaluateAll((nodes) => nodes.map((node) => node.textContent ?? ''));
	expect(
		liveTexts.join('\n'),
		'live regions must not carry account, token or catalog internals'
	).not.toMatch(/token|bearer|password|secret|api[_ \-]?key|session|nzb|usenet|browse-alpha|browse-beta|browse-gamma|browse-delta/i);
});

test('BA-B-10 existing zero-assertive suite rules still hold after browse actions', async ({
	page
}) => {
	await mockLibrary(page);
	await openLibrary(page);
	const field = await requireFind(page);
	await field.fill('song');
	await expectView(page, ['browse-alpha', 'browse-beta', 'browse-delta']);
	await orderSelect(page).selectOption({ label: 'Title A–Z' });
	await viewSelect(page).selectOption({ label: 'Favorites only' });
	await viewSelect(page).selectOption({ label: 'All tracks' });
	await page.getByRole('checkbox', { name: 'Select Beta Song', exact: true }).check();
	await page.getByRole('button', { name: 'Clear selection', exact: true }).click();
	await clearSearch(page).click();
	await expect(field).toHaveValue('');
	await expectZeroAssertive(page, 'browse actions');
});

test('BA-B-11 narrow light/dark with reduced motion still announces outcomes', async ({
	page
}) => {
	await mockLibrary(page);
	await page.setViewportSize({ width: 320, height: 700 });
	await page.emulateMedia({ reducedMotion: 'reduce', colorScheme: 'light' });
	await openLibrary(page);
	const order = orderSelect(page);
	await expect(order.locator('option')).toHaveCount(5);
	const view = viewSelect(page);
	await expect(view.locator('option')).toHaveCount(3);
	await order.selectOption({ label: 'Title A–Z' });
	await expectOutcomeOnce(page, 'List ordered: Title A–Z');
	await page.emulateMedia({ reducedMotion: 'reduce', colorScheme: 'dark' });
	await view.selectOption({ label: 'Favorites only' });
	await expectOutcomeOnce(page, 'Showing Favorites only');
	const overflow = await page.evaluate(
		() => document.documentElement.scrollWidth - document.documentElement.clientWidth
	);
	expect(overflow, 'narrow library must not scroll horizontally').toBeLessThanOrEqual(1);
	await expectZeroAssertive(page, 'narrow reduced-motion browse');
});
