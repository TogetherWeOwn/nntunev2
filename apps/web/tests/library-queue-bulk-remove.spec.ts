import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Acceptance: remove an explicitly selected subset from the queue without
 * disturbing surviving playback.
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build lands the
 * bulk-remove wiring on top of this branch and must make these tests pass
 * WITHOUT editing this file; a wrong test comes back to the acceptance lane
 * instead.
 *
 * Harness note (same as the neighbouring library specs): `/library` is
 * client-fetched (`ssr = false`), so the click-driven contract is pinned here
 * in the offline Playwright suite with a route-mocked `/api/library`. Every
 * test guards its first new-control interaction with an explicit visibility
 * assertion, so a run against a build without the controls fails as an
 * assertion (missing control), never as a click error. Fully offline and
 * deterministic: no backend, no provider, no spend.
 *
 * Hooks (new component-owned literals, not TESTIDS entries — the same rule
 * as `queue-item` / `now-playing`, which live in a component):
 * - One native `checkbox` per queue row, scoped inside
 *   `[data-testid="queue-item"][data-track-id="<id>"]`, with a per-track
 *   accessible name `Select {title} for removal` (for example
 *   `Select Zulu Song for removal`). Native keyboard/touch behavior. The
 *   `for removal` suffix keeps it distinct from the library `Select {title}`
 *   checkbox and the queue `Select {title}` direct-selection button.
 * - `queue-bulk-count` (`data-testid`) showing `<n> selected for removal`
 *   (for example `2 selected for removal`), always rendered while the queue
 *   panel is mounted.
 * - `queue-bulk-hidden-count` (`data-testid`) only when a queue filter hides
 *   at least one selected row, naming the hidden selected count and the
 *   filter (for example `1 selected track hidden by filter.`).
 * - `Remove selected` and `Clear queue selection` buttons (exact accessible
 *   names), the former disabled while nothing is selected. `Clear queue
 *   selection` is distinct from the library `Clear selection`.
 * - The existing `queue-announcement` polite region (`role="status"`)
 *   reports one aggregate `Removed <n> <track|tracks> from the queue.` per
 *   activation. No new live region, no per-row chatter.
 * There is deliberately NO `Select all` / bulk reorder / undo control and no
 * new route: each checkbox is an explicit user choice.
 *
 * Contract under test (paired build):
 * - Checking never removes, plays, seeks, fetches, favorites-mutates,
 *   persists or crosses accounts; checkbox activation never bubbles into the
 *   row Remove/move/play-next/select/star actions. Immediate row Remove and
 *   star keep working.
 * - Queue selection is transient and distinct from library selection and from
 *   the current-track selection. Filtered-out selected IDs stay selected and
 *   are disclosed via the hidden count. Clear empties the pending set only.
 * - Remove selected removes the activation snapshot atomically in one queue
 *   state transition through the existing dedupe-free removal: survivors keep
 *   arranged order and object identity, the current ID/source survives exactly
 *   when it survives, otherwise the first surviving successor in the original
 *   order becomes current, otherwise the last surviving predecessor, otherwise
 *   the queue empties (current cleared). Never calls play, never implicitly
 *   activates media, never renews a grant or fetches a source.
 * - Removed/missing selection IDs prune after removal, refresh, clear and
 *   external queue changes. Unselected rows/favorites/library selection stay
 *   unchanged; repeated removal is a no-op. Account/auth teardown clears the
 *   transient queue selection and stale result feedback.
 * - One-row Remove, trim, top/bottom, move, play-next and queued-search
 *   contracts are preserved. Default consumers without the optional bulk
 *   callbacks render no bulk UI, including the demo player.
 * - After removal, focus moves only when the removed node owned focus, to the
 *   next surviving applicable control (first surviving row checkbox) or to the
 *   `queue-empty-focus` target when nothing survives; a newer user focus
 *   choice is respected and background updates never steal focus.
 *
 * Case IDs (stable): QUEUEMULTI-00 control, QUEUEMULTI-01 render,
 * QUEUEMULTI-02 check purity, QUEUEMULTI-03 no bubble, QUEUEMULTI-04 keyboard,
 * QUEUEMULTI-05 remove around current, QUEUEMULTI-06 successor fallback,
 * QUEUEMULTI-07 predecessor fallback, QUEUEMULTI-08 remove all + empty focus,
 * QUEUEMULTI-09 repeat no-op, QUEUEMULTI-10 clear pending only,
 * QUEUEMULTI-11 hidden selected, QUEUEMULTI-12 prune on one-row remove,
 * QUEUEMULTI-13 refresh prune/keep, QUEUEMULTI-14 auth teardown,
 * QUEUEMULTI-15 access-refusal purge, QUEUEMULTI-16 library isolation,
 * QUEUEMULTI-17 current-track distinct, QUEUEMULTI-18 preserved contracts,
 * QUEUEMULTI-19 demo unchanged, QUEUEMULTI-20 focus recovery,
 * QUEUEMULTI-21 narrow both-theme touch, QUEUEMULTI-22 aggregate result.
 * QUEUEMULTI-U-* live in the sibling bulk-remove unit acceptance file.
 *
 * Out of scope (not pinned): Select all/displayed, bulk queue reordering,
 * undo, persisted queue selection, server playlists, cross-device queues,
 * backend search, audio transport, grant/auth changes, deployed streaming.
 */

test.use({ viewport: { width: 1000, height: 700 }, hasTouch: true });
test.setTimeout(20_000);

// Rendered order is API order; ids are deliberately not in sorted order.
// Titles/artists avoid "star", "favorite", "select", "append", "clear",
// "remove" and "filter" so name-filtered locators below can only match their
// intended control. Five rows prove non-contiguous middle survival (first +
// last removed, middle survives) rather than an adjacent toggle.
const ITEMS = [
	{ id: 'row-zulu', title: 'Zulu Song', artist: 'Harbor Lights' },
	{ id: 'row-alpha', title: 'Alpha Song' },
	{ id: 'row-mike', title: 'Mike Song', artist: 'Harbor Lights' },
	{ id: 'row-bravo', title: 'Bravo Song', artist: 'North Field' },
	{ id: 'row-kilo', title: 'Kilo Song', artist: 'North Field' }
];

const NEXT_ACCOUNT = [{ id: 'row-nova', title: 'Nova Song', artist: 'Next Account' }];

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

async function seedLibrary(
	page: Page,
	payload: { current: unknown } = { current: ITEMS }
): Promise<void> {
	await routeLibrary(page, payload);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(
		(payload.current as unknown[]).length
	);
}

/** Fill the queue by clicking library rows in the given gesture order. */
async function seedQueueInOrder(page: Page, ids: string[]): Promise<void> {
	await seedLibrary(page);
	for (const id of ids) {
		await row(page, id).click();
	}
	await expect(page.getByTestId('queue-item')).toHaveCount(ids.length);
}

function row(page: Page, id: string): Locator {
	return page.locator(`[data-testid="library-item"][data-id="${id}"]`);
}

function queueItem(page: Page, id: string): Locator {
	return page.locator(
		`[data-testid="queue-item"][data-track-id="${id}"], [data-testid="queue-item"][data-id="${id}"]`
	);
}

function queueCheckbox(page: Page, id: string): Locator {
	return queueItem(page, id).getByRole('checkbox');
}

function libraryCheckbox(page: Page, id: string): Locator {
	return row(page, id).getByRole('checkbox');
}

function starButton(page: Page, id: string): Locator {
	return row(page, id).getByRole('button', { name: /star|favorite/i });
}

function queueStarButton(page: Page, id: string): Locator {
	return queueItem(page, id).getByRole('button', { name: /star|favorite/i });
}

/** A queue management button by its accessible name, scoped to one queue row. */
function queueRowButton(page: Page, trackId: string, name: string): Locator {
	return queueItem(page, trackId).getByRole('button', { name, exact: true });
}

function bulkCount(page: Page): Locator {
	return page.getByTestId('queue-bulk-count');
}

function bulkHiddenCount(page: Page): Locator {
	return page.getByTestId('queue-bulk-hidden-count');
}

function removeSelectedButton(page: Page): Locator {
	return page.getByRole('button', { name: 'Remove selected', exact: true });
}

function clearQueueSelectionButton(page: Page): Locator {
	return page.getByRole('button', { name: 'Clear queue selection', exact: true });
}

function announcement(page: Page): Locator {
	return page.getByTestId('queue-announcement');
}

function emptyTarget(page: Page): Locator {
	return page.getByTestId('queue-empty-focus');
}

function filterInput(page: Page): Locator {
	return page
		.getByRole('textbox', { name: 'Filter queue', exact: true })
		.or(page.getByRole('searchbox', { name: 'Filter queue', exact: true }));
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

async function settle(page: Page): Promise<void> {
	await page.evaluate(
		() =>
			new Promise<void>((resolve) =>
				requestAnimationFrame(() => requestAnimationFrame(() => resolve()))
			)
	);
}

/** Catch accidental media activation, source writes and seek writes. */
async function instrument(page: Page): Promise<void> {
	await page.addInitScript(() => {
		const probe = (window.__queueBulkProbe = {
			media: [] as string[],
			sources: [] as string[],
			positions: [] as number[]
		});
		for (const method of ['play', 'pause', 'load'] as const) {
			Object.defineProperty(HTMLMediaElement.prototype, method, {
				configurable: true,
				value() {
					probe.media.push(method);
					return method === 'play' ? Promise.resolve() : undefined;
				}
			});
		}
		const currentTime = Object.getOwnPropertyDescriptor(
			HTMLMediaElement.prototype,
			'currentTime'
		)!;
		Object.defineProperty(HTMLMediaElement.prototype, 'currentTime', {
			...currentTime,
			set(value: number) {
				probe.positions.push(Number(value));
				currentTime.set!.call(this, value);
			}
		});
		for (const prototype of [HTMLMediaElement.prototype, HTMLSourceElement.prototype]) {
			const original = Object.getOwnPropertyDescriptor(prototype, 'src')!;
			Object.defineProperty(prototype, 'src', {
				...original,
				set(value: string) {
					probe.sources.push(String(value));
					original.set!.call(this, value);
				}
			});
		}
		const create = Document.prototype.createElement;
		Document.prototype.createElement = function (...args: Parameters<typeof create>) {
			if (/^(audio|source)$/i.test(args[0])) probe.sources.push(`create:${args[0]}`);
			return Reflect.apply(create, this, args);
		};
		const attribute = Element.prototype.setAttribute;
		Element.prototype.setAttribute = function (name, value) {
			if (name.toLowerCase() === 'src' && this.matches('audio, video, source'))
				probe.sources.push(String(value));
			return attribute.call(this, name, value);
		};
	});
}

async function mediaProbe(page: Page): Promise<{
	media: string[];
	sources: string[];
	positions: number[];
}> {
	return page.evaluate(
		() =>
			(window as unknown as { __queueBulkProbe: { media: string[]; sources: string[]; positions: number[] } })
				.__queueBulkProbe ?? { media: [], sources: [], positions: [] }
	);
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

/** The recovered checkbox is usable, visible and keeps a focus ring. */
async function expectUsableCheckbox(target: Locator): Promise<void> {
	await expect(target).toBeFocused();
	await expect(target).toBeVisible();
	await expect(target).toHaveAttribute('type', 'checkbox');
	await expect(target).toBeEnabled();
	const ring = await target.evaluate((el) => {
		const css = getComputedStyle(el);
		return (
			(css.outlineStyle !== 'none' && parseFloat(css.outlineWidth) > 0) ||
			css.boxShadow !== 'none'
		);
	});
	expect(ring).toBe(true);
}

/** The empty-queue target is focused, visible and programmatically focusable. */
async function expectEmptyRecovery(page: Page): Promise<void> {
	const target = emptyTarget(page);
	await expect(target).toBeFocused();
	await expect(target).toBeVisible();
	await expect(target).toHaveAttribute('tabindex', '-1');
	const tag = await target.evaluate((el) => el.tagName);
	expect(tag).not.toMatch(/^(BUTTON|A|INPUT|SELECT|TEXTAREA)$/);
	expect(((await target.textContent()) ?? '').trim()).toMatch(/queue|empty|clear/i);
	const identity = await activeIdentity(page);
	expect(identity.tag).not.toBe('BODY');
}

/** Record one entry per live-region commit so "announced once" is countable. */
async function watchAnnouncements(page: Page): Promise<void> {
	await page.evaluate(() => {
		const region = document.querySelector('[data-testid="queue-announcement"]');
		const watched = window as unknown as {
			__qmCommits: string[];
			__qmObserver?: MutationObserver;
		};
		watched.__qmObserver?.disconnect();
		watched.__qmCommits = [];
		if (!region) return;
		watched.__qmObserver = new MutationObserver(() => {
			watched.__qmCommits.push(region.textContent ?? '');
		});
		watched.__qmObserver.observe(region, { childList: true, characterData: true, subtree: true });
	});
}

async function announcementCommits(page: Page): Promise<string[]> {
	return page.evaluate(
		() => (window as unknown as { __qmCommits?: string[] }).__qmCommits ?? []
	);
}

async function fireAuthTransition(page: Page): Promise<void> {
	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate((key) => {
		window.localStorage.setItem(key, 'test-queue-bulk-auth-transition');
		window.localStorage.removeItem(key);
	}, 'nntune.library.auth-transition.v1');
	await otherTab.close();
}

test('[QUEUEMULTI-00] control: the library queues in gesture order and one-row Remove preserves current', async ({
	page
}) => {
	await seedQueueInOrder(page, ['row-mike', 'row-zulu', 'row-alpha']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu', 'row-alpha']);
	expect(await ariaCurrent(queueItem(page, 'row-mike'))).toBe('true');

	const remove = queueRowButton(page, 'row-zulu', 'Remove Zulu Song from queue');
	await expect(remove).toBeVisible();
	await remove.click();

	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
	expect(await ariaCurrent(queueItem(page, 'row-mike'))).toBe('true');
});

test('[QUEUEMULTI-01] each queue row carries a labeled bulk checkbox with counts and actions, remove disabled while empty', async ({
	page
}) => {
	await seedQueueInOrder(page, ['row-mike', 'row-zulu', 'row-alpha', 'row-bravo']);

	for (const id of ['row-mike', 'row-zulu', 'row-alpha', 'row-bravo']) {
		const title = ITEMS.find((item) => item.id === id)!.title;
		const box = queueCheckbox(page, id);
		await expect(box, `bulk checkbox missing on queue row ${id}`).toBeVisible();
		await expect(box).toHaveAttribute('type', 'checkbox');
		await expect(box).not.toBeChecked();
		await expect(box).toHaveAccessibleName(`Select ${title} for removal`);
	}

	await expect(bulkCount(page), 'queue bulk count missing').toBeVisible();
	await expect(bulkCount(page)).toContainText('0 selected for removal');
	await expect(clearQueueSelectionButton(page), 'Clear queue selection missing').toBeVisible();
	await expect(removeSelectedButton(page), 'Remove selected missing').toBeVisible();
	await expect(removeSelectedButton(page)).toBeDisabled();

	// No bulk-select escape hatch, no bulk reorder, no undo, no new route.
	await expect(page.getByRole('button', { name: /select all|select displayed/i })).toHaveCount(0);
	await expect(
		page.getByRole('checkbox', { name: /select all|select displayed/i })
	).toHaveCount(0);
	await expect(page.getByRole('button', { name: /move selected|reorder selected/i })).toHaveCount(0);
	await expect(page.getByRole('button', { name: /undo remove/i })).toHaveCount(0);
	expect(page.url()).toContain('/library');
});

test('[QUEUEMULTI-02] checking bulk boxes removes, plays, fetches and stars nothing', async ({
	page
}) => {
	const requests: { method: string; url: string }[] = [];
	page.on('request', (request) => requests.push({ method: request.method(), url: request.url() }));
	await instrument(page);
	await seedQueueInOrder(page, ['row-mike', 'row-zulu', 'row-alpha', 'row-bravo']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');

	const first = queueCheckbox(page, 'row-zulu');
	await expect(first, 'bulk checkbox missing on row-zulu').toBeVisible();
	await first.check();
	await queueCheckbox(page, 'row-bravo').check();

	await expect(bulkCount(page)).toContainText('2 selected for removal');
	await expect(page.getByTestId('queue-item')).toHaveCount(4);
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu', 'row-alpha', 'row-bravo']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
	expect(await ariaCurrent(queueItem(page, 'row-mike'))).toBe('true');
	await expect(announcement(page)).not.toContainText(/removed/i);
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
	for (const item of ITEMS) {
		const star = starButton(page, item.id);
		if ((await star.count()) > 0) await expect(star).toHaveAttribute('aria-pressed', 'false');
	}
	const probe = await mediaProbe(page);
	expect(probe.media, 'checking must not touch media transport').toEqual([]);
	expect(probe.sources, 'checking must not write a media source').toEqual([]);
	expect(probe.positions, 'checking must not seek').toEqual([]);

	const libraryGets = requests.filter(
		(request) => request.method === 'GET' && request.url.includes('/api/library')
	);
	expect(libraryGets, 'checking must not refetch the catalog').toHaveLength(1);
	expect(
		requests.filter((request) => request.method !== 'GET'),
		'checking must not mutate over the network'
	).toEqual([]);
});

test('[QUEUEMULTI-03] bulk checkbox activation never bubbles into row Remove, move, play-next, select or star', async ({
	page
}) => {
	await seedQueueInOrder(page, ['row-mike', 'row-zulu', 'row-alpha']);
	const box = queueCheckbox(page, 'row-alpha');
	await expect(box, 'bulk checkbox missing on row-alpha').toBeVisible();
	// Click the checkbox input itself (not the label): row actions stay untouched.
	await box.click();

	await expect(box).toBeChecked();
	await expect(bulkCount(page)).toContainText('1 selected for removal');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu', 'row-alpha']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
	await expect(queueStarButton(page, 'row-alpha')).toHaveAttribute('aria-pressed', 'false');
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
});

test('[QUEUEMULTI-04] bulk checkboxes are keyboard operable with stable focus', async ({
	page
}) => {
	await seedQueueInOrder(page, ['row-mike', 'row-zulu', 'row-alpha']);
	const box = queueCheckbox(page, 'row-zulu');
	await expect(box, 'bulk checkbox missing on row-zulu').toBeVisible();
	await box.focus();
	await expect(box).toBeFocused();

	await page.keyboard.press('Space');
	await expect(box).toBeChecked();
	await expect(box, 'focus must stay on the toggled bulk checkbox').toBeFocused();
	await expect(bulkCount(page)).toContainText('1 selected for removal');

	await page.keyboard.press('Space');
	await expect(box).not.toBeChecked();
	await expect(box).toBeFocused();
	await expect(bulkCount(page)).toContainText('0 selected for removal');

	// Tab reaches another control: nothing is tabindex-removed.
	await page.keyboard.press('Tab');
	const focused = await page.evaluate(() => {
		const el = document.activeElement as HTMLElement | null;
		return {
			tag: el?.tagName ?? '',
			name: el?.getAttribute('aria-label') ?? el?.textContent ?? ''
		};
	});
	expect(['BUTTON', 'INPUT']).toContain(focused.tag);
	expect(focused.name.trim().length).toBeGreaterThan(0);
});

test('[QUEUEMULTI-05] remove a non-current subset preserves survivor order and exact current without play or fetch', async ({
	page
}) => {
	const requests: { method: string; url: string }[] = [];
	page.on('request', (request) => requests.push({ method: request.method(), url: request.url() }));
	await instrument(page);
	// Queue order: mike (current), zulu, alpha, bravo, kilo. Remove the
	// non-contiguous zulu + bravo; middle alpha and tail kilo survive in place.
	await seedQueueInOrder(page, ['row-mike', 'row-zulu', 'row-alpha', 'row-bravo', 'row-kilo']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');

	const first = queueCheckbox(page, 'row-zulu');
	await expect(first, 'bulk checkbox missing on row-zulu').toBeVisible();
	await first.check();
	await queueCheckbox(page, 'row-bravo').check();
	await expect(bulkCount(page)).toContainText('2 selected for removal');

	await watchAnnouncements(page);
	const remove = removeSelectedButton(page);
	await expect(remove).toBeEnabled();
	await remove.click();

	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha', 'row-kilo']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
	expect(await ariaCurrent(queueItem(page, 'row-mike'))).toBe('true');
	expect(await ariaCurrent(queueItem(page, 'row-alpha'))).toBeNull();

	const region = announcement(page);
	await expect(region, 'bulk removal must announce').toContainText('Removed 2 tracks from the queue.');
	await expect(region).toHaveAttribute('role', 'status');
	expect(await announcementCommits(page)).toHaveLength(1);

	// Selection prunes to empty; the button disables without further chatter.
	await expect(bulkCount(page)).toContainText('0 selected for removal');
	await expect(removeSelectedButton(page)).toBeDisabled();
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
	const probe = await mediaProbe(page);
	expect(probe.media, 'bulk removal must not touch media transport').toEqual([]);
	expect(probe.sources, 'bulk removal must not fetch a source').toEqual([]);
	expect(probe.positions, 'bulk removal must not seek').toEqual([]);
	expect(
		requests.filter((request) => request.method !== 'GET'),
		'bulk removal must not mutate over the network'
	).toEqual([]);
});

test('[QUEUEMULTI-06] removing the current track selects the first surviving successor', async ({
	page
}) => {
	// Queue order: mike, zulu, alpha, bravo. Advance current to zulu, then
	// remove zulu + alpha; the first survivor after the removed current is
	// bravo (not the predecessor mike).
	await seedQueueInOrder(page, ['row-mike', 'row-zulu', 'row-alpha', 'row-bravo']);
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');

	const current = queueCheckbox(page, 'row-zulu');
	await expect(current, 'bulk checkbox missing on row-zulu').toBeVisible();
	await current.check();
	await queueCheckbox(page, 'row-alpha').check();
	await removeSelectedButton(page).click();

	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-bravo']);
	await expect(page.getByTestId('now-playing')).toContainText('Bravo Song');
	expect(await ariaCurrent(queueItem(page, 'row-bravo'))).toBe('true');
	await expect(announcement(page)).toContainText('Removed 2 tracks from the queue.');
	await expect(bulkCount(page)).toContainText('0 selected for removal');
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
});

test('[QUEUEMULTI-07] removing the current track and every successor selects the last surviving predecessor', async ({
	page
}) => {
	// Queue order: mike, zulu, alpha, bravo. Advance current to alpha, then
	// remove alpha + bravo; no successor survives, so the last predecessor
	// (zulu) becomes current.
	await seedQueueInOrder(page, ['row-mike', 'row-zulu', 'row-alpha', 'row-bravo']);
	await page.getByTestId('player-next').click();
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Alpha Song');

	const current = queueCheckbox(page, 'row-alpha');
	await expect(current, 'bulk checkbox missing on row-alpha').toBeVisible();
	await current.check();
	await queueCheckbox(page, 'row-bravo').check();
	await removeSelectedButton(page).click();

	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu']);
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');
	expect(await ariaCurrent(queueItem(page, 'row-zulu'))).toBe('true');
	await expect(announcement(page)).toContainText('Removed 2 tracks from the queue.');
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
});

test('[QUEUEMULTI-08] removing every track empties truthfully, clears current and focuses the empty target', async ({
	page
}) => {
	await seedQueueInOrder(page, ['row-mike', 'row-zulu', 'row-alpha']);
	for (const id of ['row-mike', 'row-zulu', 'row-alpha']) {
		const box = queueCheckbox(page, id);
		await expect(box, `bulk checkbox missing on ${id}`).toBeVisible();
		await box.check();
	}
	await expect(bulkCount(page)).toContainText('3 selected for removal');

	const remove = removeSelectedButton(page);
	await remove.focus();
	await expect(remove).toBeFocused();
	await page.keyboard.press('Enter');

	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
	await expect(page.getByTestId('queue-position')).toContainText('Queue selection: Queue is empty');
	await expect(page.getByTestId('player-next')).toBeDisabled();
	await expect(page.getByRole('button', { name: 'Clear queue', exact: true })).toBeDisabled();
	await expect(announcement(page)).toContainText('Removed 3 tracks from the queue.');
	await expect(bulkCount(page)).toContainText('0 selected for removal');
	await expect(removeSelectedButton(page)).toBeDisabled();
	await expectEmptyRecovery(page);
});

test('[QUEUEMULTI-09] repeated removal after a prune is a no-op without chatter', async ({
	page
}) => {
	await seedQueueInOrder(page, ['row-mike', 'row-zulu', 'row-alpha', 'row-bravo']);
	const box = queueCheckbox(page, 'row-zulu');
	await expect(box, 'bulk checkbox missing on row-zulu').toBeVisible();
	await box.check();
	await queueCheckbox(page, 'row-bravo').check();
	await removeSelectedButton(page).click();

	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha']);
	const first = await announcement(page).textContent();

	await watchAnnouncements(page);
	await expect(removeSelectedButton(page)).toBeDisabled();
	// A disabled activation cannot run: force-dispatch proves the host treats
	// the pruned snapshot as a no-op rather than re-announcing.
	await removeSelectedButton(page).click({ force: true });
	await settle(page);

	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
	await expect(announcement(page)).toHaveText(first ?? '');
	expect(await announcementCommits(page)).toEqual([]);
});

test('[QUEUEMULTI-10] clear queue selection empties the pending set only', async ({
	page
}) => {
	await seedQueueInOrder(page, ['row-mike', 'row-zulu', 'row-alpha']);
	await starButton(page, 'row-bravo').click();
	await expect(page.getByTestId('favorite-item')).toHaveCount(1);

	const box = queueCheckbox(page, 'row-zulu');
	await expect(box, 'bulk checkbox missing on row-zulu').toBeVisible();
	await box.check();
	await queueCheckbox(page, 'row-alpha').check();
	await expect(bulkCount(page)).toContainText('2 selected for removal');

	// Library selection stays independent of the pending queue set.
	await libraryCheckbox(page, 'row-kilo').check();
	await expect(page.getByTestId('selection-count')).toContainText('1 selected');

	await clearQueueSelectionButton(page).click();

	await expect(bulkCount(page)).toContainText('0 selected for removal');
	for (const id of ['row-mike', 'row-zulu', 'row-alpha']) {
		await expect(queueCheckbox(page, id)).not.toBeChecked();
	}
	await expect(removeSelectedButton(page)).toBeDisabled();
	// Queue, favorites and library selection are untouched by the pending-only clear.
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu', 'row-alpha']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
	await expect(page.getByTestId('favorite-item')).toHaveCount(1);
	await expect(page.getByTestId('selection-count')).toContainText('1 selected');
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
});

test('[QUEUEMULTI-11] filtered-out selected rows stay selected, disclose a hidden count and remove with the snapshot', async ({
	page
}) => {
	await seedQueueInOrder(page, ['row-mike', 'row-zulu', 'row-alpha', 'row-bravo', 'row-kilo']);
	const zulu = queueCheckbox(page, 'row-zulu');
	await expect(zulu, 'bulk checkbox missing on row-zulu').toBeVisible();
	await zulu.check();
	await queueCheckbox(page, 'row-kilo').check();
	await expect(bulkCount(page)).toContainText('2 selected for removal');

	// Filter to "Zulu": kilo stays selected behind the filter and is disclosed.
	const input = filterInput(page);
	await expect(input).toBeVisible();
	await input.fill('Zulu');
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	expect(await queueOrder(page)).toEqual(['row-zulu']);
	await expect(bulkCount(page)).toContainText('2 selected for removal');
	const hidden = bulkHiddenCount(page);
	await expect(hidden, 'hidden selected count missing').toBeVisible();
	await expect(hidden).toContainText('1 selected');
	await expect(hidden).toContainText('hidden');
	await expect(page.getByTestId('queue-filter-count')).toContainText('1 of 5 tracks');

	await removeSelectedButton(page).click();

	// Both the visible and the hidden selected rows remove atomically.
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('queue-filter-empty')).toContainText('No matches in this queue');
	await input.fill('');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha', 'row-bravo']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
	await expect(announcement(page)).toContainText('Removed 2 tracks from the queue.');
	await expect(bulkCount(page)).toContainText('0 selected for removal');
	await expect(bulkHiddenCount(page)).toHaveCount(0);
});

test('[QUEUEMULTI-12] one-row Remove of a selected ID prunes the pending set', async ({
	page
}) => {
	await seedQueueInOrder(page, ['row-mike', 'row-zulu', 'row-alpha', 'row-bravo']);
	const zulu = queueCheckbox(page, 'row-zulu');
	await expect(zulu, 'bulk checkbox missing on row-zulu').toBeVisible();
	await zulu.check();
	await queueCheckbox(page, 'row-bravo').check();
	await expect(bulkCount(page)).toContainText('2 selected for removal');

	// External queue change through the existing one-row path prunes that ID.
	await queueRowButton(page, 'row-zulu', 'Remove Zulu Song from queue').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha', 'row-bravo']);
	await expect(bulkCount(page)).toContainText('1 selected for removal');
	await expect(queueCheckbox(page, 'row-bravo')).toBeChecked();

	// The remaining snapshot still removes exactly once.
	await removeSelectedButton(page).click();
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha']);
	await expect(bulkCount(page)).toContainText('0 selected for removal');
});

test('[QUEUEMULTI-13] same-ID refresh keeps the pending set while a missing-ID refresh prunes queue and selection', async ({
	page
}) => {
	const payload: { current: unknown } = { current: ITEMS };
	await routeLibrary(page, payload);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	for (const id of ['row-mike', 'row-zulu', 'row-alpha', 'row-bravo']) {
		await row(page, id).click();
	}
	await expect(page.getByTestId('queue-item')).toHaveCount(4);

	const zulu = queueCheckbox(page, 'row-zulu');
	await expect(zulu, 'bulk checkbox missing on row-zulu').toBeVisible();
	await zulu.check();
	await queueCheckbox(page, 'row-bravo').check();
	await expect(bulkCount(page)).toContainText('2 selected for removal');

	// Same-account refresh with identical IDs keeps every choice.
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(bulkCount(page)).toContainText('2 selected for removal');
	await expect(queueCheckbox(page, 'row-zulu')).toBeChecked();
	await expect(queueCheckbox(page, 'row-bravo')).toBeChecked();

	// A refresh whose snapshot drops bravo prunes both the queue and the set.
	payload.current = ITEMS.filter((item) => item.id !== 'row-bravo');
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length - 1);
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu', 'row-alpha']);
	await expect(bulkCount(page)).toContainText('1 selected for removal');
	await expect(queueCheckbox(page, 'row-zulu')).toBeChecked();
});

test('[QUEUEMULTI-14] auth transition clears the transient queue selection and stale result feedback', async ({
	page
}) => {
	const payload: { current: unknown } = { current: ITEMS };
	await routeLibrary(page, payload);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	for (const id of ['row-mike', 'row-zulu', 'row-alpha']) {
		await row(page, id).click();
	}
	const zulu = queueCheckbox(page, 'row-zulu');
	await expect(zulu, 'bulk checkbox missing on row-zulu').toBeVisible();
	await zulu.check();
	await removeSelectedButton(page).click();
	await expect(announcement(page)).toContainText('Removed 1 track from the queue.');

	// A fresh pending choice plus the stale result must both clear at the boundary.
	await queueCheckbox(page, 'row-alpha').check();
	await expect(bulkCount(page)).toContainText('1 selected for removal');

	await fireAuthTransition(page);
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);

	payload.current = NEXT_ACCOUNT;
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(NEXT_ACCOUNT.length);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	// No leaked selection, no stale bulk feedback, no leaked queue.
	await expect(page.getByTestId('queue-bulk-count')).toHaveCount(0);
	await expect(removeSelectedButton(page)).toHaveCount(0);
	await expect(announcement(page)).not.toContainText(/removed/i);
});

test('[QUEUEMULTI-15] access refusal purges the pending queue selection instead of leaking it', async ({
	page
}) => {
	const payload: { current: unknown } = { current: ITEMS };
	await routeLibrary(page, payload);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	for (const id of ['row-mike', 'row-zulu', 'row-alpha']) {
		await row(page, id).click();
	}
	const zulu = queueCheckbox(page, 'row-zulu');
	await expect(zulu, 'bulk checkbox missing on row-zulu').toBeVisible();
	await zulu.check();
	await expect(bulkCount(page)).toContainText('1 selected for removal');

	await page.unroute('**/api/library');
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 401,
			contentType: 'application/json',
			body: JSON.stringify({ error: 'unauthorized' })
		})
	);
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.getByRole('alert')).toContainText(/signed out|do not have access/i);

	await page.unroute('**/api/library');
	payload.current = NEXT_ACCOUNT;
	await routeLibrary(page, payload);
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expect(page.getByTestId('library-list')).toContainText('Nova Song');
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('queue-bulk-count')).toHaveCount(0);
	await expect(removeSelectedButton(page)).toHaveCount(0);
});

test('[QUEUEMULTI-16] queue selection stays distinct from library selection in both directions', async ({
	page
}) => {
	await seedQueueInOrder(page, ['row-mike', 'row-zulu', 'row-alpha', 'row-bravo']);

	// Library pending set: mike + kilo (kilo is not queued).
	await libraryCheckbox(page, 'row-mike').check();
	await libraryCheckbox(page, 'row-kilo').check();
	await expect(page.getByTestId('selection-count')).toContainText('2 selected');

	// Queue pending set: zulu + bravo. Counts never mirror each other.
	const zulu = queueCheckbox(page, 'row-zulu');
	await expect(zulu, 'bulk checkbox missing on row-zulu').toBeVisible();
	await zulu.check();
	await queueCheckbox(page, 'row-bravo').check();
	await expect(bulkCount(page)).toContainText('2 selected for removal');
	await expect(page.getByTestId('selection-count')).toContainText('2 selected');

	// Library append keeps the queue set untouched.
	await page.getByRole('button', { name: 'Append selected', exact: true }).click();
	await expect(page.getByTestId('queue-item')).toHaveCount(5);
	await expect(bulkCount(page)).toContainText('2 selected for removal');
	await expect(queueCheckbox(page, 'row-zulu')).toBeChecked();

	// Queue removal keeps the library set untouched.
	await removeSelectedButton(page).click();
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha', 'row-kilo']);
	await expect(page.getByTestId('selection-count')).toContainText('2 selected');
	await expect(libraryCheckbox(page, 'row-mike')).toBeChecked();
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
});

test('[QUEUEMULTI-17] bulk selection never changes the current track until removal', async ({
	page
}) => {
	await seedQueueInOrder(page, ['row-mike', 'row-zulu', 'row-alpha', 'row-bravo']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');

	// Direct selection moves current without touching the bulk set.
	const select = queueRowButton(page, 'row-alpha', 'Select Alpha Song');
	await expect(select).toBeVisible();
	const bulk = queueCheckbox(page, 'row-zulu');
	await expect(bulk, 'bulk checkbox missing on row-zulu').toBeVisible();
	await bulk.check();
	await queueCheckbox(page, 'row-bravo').check();
	await select.click();

	await expect(page.getByTestId('now-playing')).toContainText('Alpha Song');
	expect(await ariaCurrent(queueItem(page, 'row-alpha'))).toBe('true');
	await expect(bulkCount(page)).toContainText('2 selected for removal');
	await expect(queueCheckbox(page, 'row-zulu')).toBeChecked();

	// Bulk checking a third row still leaves current alone.
	await queueCheckbox(page, 'row-mike').check();
	await expect(page.getByTestId('now-playing')).toContainText('Alpha Song');
	expect(await ariaCurrent(queueItem(page, 'row-alpha'))).toBe('true');
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu', 'row-alpha', 'row-bravo']);
});

test('[QUEUEMULTI-18] one-row Remove, move, top/bottom, play-next, trim and queued search keep working', async ({
	page
}) => {
	await seedQueueInOrder(page, ['row-mike', 'row-zulu', 'row-alpha', 'row-bravo']);
	// Move up reorders and keeps the current track playing.
	await queueRowButton(page, 'row-alpha', 'Move Alpha Song up').click();
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha', 'row-zulu', 'row-bravo']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');

	// Play next inserts immediately after current without duplicating.
	await queueRowButton(page, 'row-bravo', 'Play Bravo Song next').click();
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-bravo', 'row-alpha', 'row-zulu']);

	// Top/bottom jumps keep the current identity.
	await queueRowButton(page, 'row-zulu', 'Move Zulu Song to top').click();
	expect(await queueOrder(page)).toEqual(['row-zulu', 'row-mike', 'row-bravo', 'row-alpha']);
	expect(await ariaCurrent(queueItem(page, 'row-mike'))).toBe('true');

	// Queued search filters presentation only.
	await expect(filterInput(page)).toBeVisible();
	await filterInput(page).fill('Bravo');
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	await page.getByRole('button', { name: 'Clear queue filter', exact: true }).click();
	await expect(page.getByTestId('queue-item')).toHaveCount(4);

	// Trim keeps current and upcoming, dropping the prefix. Current is mike
	// at index 1 (queue is zulu, mike, bravo, alpha), so Keep drops zulu.
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
	await page.getByRole('button', { name: 'Keep current and upcoming', exact: true }).click();
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-bravo', 'row-alpha']);

	// One-row Remove still drops exactly one row.
	await queueRowButton(page, 'row-bravo', 'Remove Bravo Song from queue').click();
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
});

test('[QUEUEMULTI-19] the demo player exposes no bulk controls', async ({ page }) => {
	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(page.getByTestId('queue-bulk-count')).toHaveCount(0);
	await expect(page.getByTestId('queue-bulk-hidden-count')).toHaveCount(0);
	await expect(removeSelectedButton(page)).toHaveCount(0);
	await expect(clearQueueSelectionButton(page)).toHaveCount(0);
	await expect(page.locator('[data-testid="queue-item"] input[type="checkbox"]')).toHaveCount(0);
	// Existing demo management still works without the optional bulk callbacks.
	await expect(page.getByRole('button', { name: 'Clear queue', exact: true })).toBeVisible();
});

test('[QUEUEMULTI-20] bulk focus moves only when the removed node owned focus and respects a newer choice', async ({
	page
}) => {
	await seedQueueInOrder(page, ['row-mike', 'row-zulu', 'row-alpha', 'row-bravo']);
	const zulu = queueCheckbox(page, 'row-zulu');
	await expect(zulu, 'bulk checkbox missing on row-zulu').toBeVisible();
	await zulu.check();
	await queueCheckbox(page, 'row-alpha').check();

	// Keyboard activation from Remove selected lands on the first surviving checkbox.
	const remove = removeSelectedButton(page);
	await remove.focus();
	await expect(remove).toBeFocused();
	await page.keyboard.press('Enter');
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	await expectUsableCheckbox(queueCheckbox(page, 'row-mike'));

	// A newer user focus choice wins: move focus to the surviving bravo
	// checkbox, select mike, remove via pointer — focus stays where the user
	// put it instead of snapping back to the first survivor.
	await queueCheckbox(page, 'row-mike').check();
	const bravo = queueCheckbox(page, 'row-bravo');
	await bravo.focus();
	await expect(bravo).toBeFocused();
	await removeSelectedButton(page).click();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	expect(await queueOrder(page)).toEqual(['row-bravo']);
	await expect(bravo).toBeFocused();
});

test('[QUEUEMULTI-21] narrow-screen light and dark keep every bulk control visible, touchable and focused', async ({
	page
}) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await instrument(page);
	await seedQueueInOrder(page, ['row-mike', 'row-zulu', 'row-alpha', 'row-bravo']);

	for (const scheme of ['light', 'dark'] as const) {
		await page.emulateMedia({ colorScheme: scheme });

		const box = queueCheckbox(page, 'row-zulu');
		await expect(box, `bulk checkbox missing on row-zulu (${scheme})`).toBeVisible();
		await expect(bulkCount(page)).toBeVisible();
		await expect(clearQueueSelectionButton(page)).toBeVisible();
		await expect(removeSelectedButton(page)).toBeVisible();

		// Visible keyboard focus in both themes.
		await box.focus();
		await expect(box).toBeFocused();
		const ring = await box.evaluate((el) => {
			const css = getComputedStyle(el);
			return (
				(css.outlineStyle !== 'none' && parseFloat(css.outlineWidth) > 0) ||
				css.boxShadow !== 'none'
			);
		});
		expect(ring, `bulk checkbox focus ring missing in ${scheme}`).toBe(true);

		const overflow = await page.evaluate(
			() => document.documentElement.scrollWidth - document.documentElement.clientWidth
		);
		expect(overflow, `horizontal overflow in ${scheme} mode`).toBeLessThanOrEqual(1);
	}

	// Touch still toggles and removes in activation order at narrow width.
	await queueCheckbox(page, 'row-alpha').tap();
	await queueCheckbox(page, 'row-mike').tap();
	await expect(bulkCount(page)).toContainText('2 selected for removal');
	await removeSelectedButton(page).tap();
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	expect(await queueOrder(page)).toEqual(['row-zulu', 'row-bravo']);
	const probe = await mediaProbe(page);
	expect(probe.media, 'touch bulk removal must not touch media').toEqual([]);
	expect(probe.sources, 'touch bulk removal must not fetch a source').toEqual([]);
});

test('[QUEUEMULTI-22] one polite aggregate result with truthful empty and no-match copy', async ({
	page
}) => {
	await seedQueueInOrder(page, ['row-mike', 'row-zulu', 'row-alpha']);
	const zulu = queueCheckbox(page, 'row-zulu');
	await expect(zulu, 'bulk checkbox missing on row-zulu').toBeVisible();
	await zulu.check();
	await queueCheckbox(page, 'row-alpha').check();

	await watchAnnouncements(page);
	await removeSelectedButton(page).click();

	// Exactly one polite commit names the aggregate, never per-row titles.
	const commits = await announcementCommits(page);
	expect(commits).toHaveLength(1);
	expect(commits[0]).toContain('Removed 2 tracks from the queue.');
	expect(commits[0]).not.toMatch(/Zulu Song|Alpha Song/);
	const region = announcement(page);
	await expect(region).toHaveAttribute('role', 'status');
	await expect(page.getByRole('alert')).toHaveCount(0);

	// Empty copy stays truthful after the survivors are removed too.
	await queueCheckbox(page, 'row-mike').check();
	await removeSelectedButton(page).click();
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
	await expect(page.getByTestId('queue-position')).toContainText('Queue selection: Queue is empty');
	await expect(announcement(page)).toContainText('Removed 1 track from the queue.');
});
