import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Acceptance: reveal the current queue track in the loaded browse list.
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build lands the
 * implementation on top of this branch and must make these tests pass
 * WITHOUT editing this file; a wrong test comes back to the acceptance
 * author (describe this file by name, never by tracker id). Other
 * protected banks stay unchanged.
 *
 * Contract under test (paired build):
 * - A browse-list action: role=button, accessible name exactly
 *   `Show current track in library`, `data-testid="library-reveal-current"`,
 *   enabled whenever the loaded list is shown. It is distinct from the
 *   queue panel's `Show current track` button, which keeps its exact name
 *   and behavior. The action carries an accessible description naming its
 *   filter-reset effect (must match /filter/i).
 * - A non-playing current-selection marker: `data-testid="library-current-marker"`
 *   inside the matching loaded row only. Its text MUST contain "current"
 *   (case-insensitive) and MUST NOT match playing words
 *   (/\bplay(ing|ed|s)?\b/i or /now playing/i). It is not interactive.
 * - One-shot polite feedback: `data-testid="library-reveal-feedback"` with
 *   role="status", empty/hidden until first activation, exactly one message
 *   per activation. Success messages name the current track; reset messages
 *   name each cleared browse filter with a clear/reset word; unavailable
 *   messages honestly name the reason (no current selection, row not in the
 *   loaded list, row gone). Examples (keywords normative, full copy free):
 *   "Showing the current track in the library.",
 *   "Cleared the Find filter to show the current track.",
 *   "Reset the Favorites view to show the current track.",
 *   "No current track selected.",
 *   "The current track is not in the loaded list."
 * - Activation targets the queue's stable current track id (never array
 *   position, visible order or checkbox selection), resets only the browse
 *   filters hiding that row (find text, Favorites/Selected view), retains chosen
 *   ordering and pending subset ids, then focuses/scrolls the row's
 *   EXISTING primary action (`button.nt-library-row`) after DOM
 *   reconciliation. Reveal never clicks that handler: no append, select,
 *   queue, favorites, media, network or storage effect beyond the explicit
 *   browse-filter reset (presentation-only, like find today).
 * - No automatic focus or scrolling on playback changes, refresh or mount;
 *   reduced-motion gets no smooth programmatic scrolling.
 *
 * Case map (stable ids, [LIBREVEAL-NN] titles):
 * - 00 control: harness health + queue-panel reveal unchanged (green now).
 * - 01 visible current row: action, marker, focus/scroll, full preservation.
 * - 02 find-hidden row: minimal reset + disclosure + ordering/subset kept.
 * - 03 Favorites-hidden row: view reset only, find untouched.
 * - 04 hidden by find and Favorites: both reset, both disclosed.
 * - 05 reordered queue/browse rows: stable-id targeting, not position.
 * - 06 empty queue: honest unavailable feedback, nothing else moves.
 * - 07 pruned current: reveal follows the surviving current id.
 * - 08 pending subset retained across reveal (incl. filter reset).
 * - 09 row disappears around the deferred focus: no stale focus, no crash.
 * - 10 auth teardown then recovery: no stale marker, honest empty-queue feedback.
 * - 11 reduced motion: focus + instant scroll, never smooth.
 * - 12 no automatic focus/scroll on next-track, refresh or mount (green now).
 * - 13 keyboard activation + visible focus on the row action.
 * - 14 narrow light/dark + touch: usable, no sideways overflow.
 * - 15 Selected-only-view-hidden row: view resets, subset ids retained.
 *
 * A current track with no row in the loaded set is unreachable through
 * shipped /library flows (refresh prunes the queue atomically and share
 * links reject unknown ids), so LIBREVEAL-07 pins the reachable
 * prune-then-reveal behavior instead; the paired build covers the
 * defensive no-match planning branch in its own unit bank.
 *
 * RED reason: every feature case fails on the missing `Show current track
 * in library` action (assertion, not infra) until the paired build lands;
 * LIBREVEAL-00 and LIBREVEAL-12 stay green throughout as harness guards.
 *
 * Canonical commands (from apps/web):
 *   pnpm check
 *   pnpm test
 *   pnpm exec playwright test tests/library-reveal-current.spec.ts
 * Deterministic offline recipe:
 *   CI=1 PUBLIC_API_BASE_URL='' pnpm --dir apps/web exec playwright test \
 *     tests/library-reveal-current.spec.ts --workers=1 --retries=0
 */

test.use({ viewport: { width: 1280, height: 800 } });

type TrackPayload = { id: string; title: string; artist: string };

const ITEMS: TrackPayload[] = [
	{ id: 'libreveal-ember', title: 'Ember Song', artist: 'North Wind' },
	{ id: 'libreveal-pine', title: 'Pine Anthem', artist: 'Still Water' },
	{ id: 'libreveal-willow', title: 'Willow Ballad', artist: 'North Wind' },
	{ id: 'libreveal-cedar', title: 'Cedar Hymn', artist: 'Low Sun' },
	{ id: 'libreveal-birch', title: 'Birch Lullaby', artist: 'Still Water' },
	{ id: 'libreveal-alder', title: 'Alder Reel', artist: 'Low Sun' }
];

/** Tall list so the current row starts below the fold for scroll cases. */
const MANY: TrackPayload[] = Array.from({ length: 40 }, (_, index) => ({
	id: `libreveal-ridge-${index}`,
	title: `Ridge Song ${['Ember', 'Pine', 'Willow', 'Cedar'][index % 4]} ${['North', 'South', 'East', 'West', 'High', 'Low', 'Still', 'Far', 'Near', 'Long'][Math.floor(index / 4)]}`,
	artist: 'Ridge Line'
}));

const REVEAL_NAME = 'Show current track in library';

const FAVORITES_AUTH_KEY = 'nntune.library.auth-context.v1';
const FAVORITES_BASE_KEY = 'nntune.library.favorites.v1';
const FAVORITES_ACCOUNT = 'libreveal-account';
const FAVORITES_KEY = `${FAVORITES_BASE_KEY}.${FAVORITES_ACCOUNT}`;

type LibraryReply = {
	items: unknown;
	status: number;
	hold: Promise<unknown> | null;
};

type ProbeSnapshot = {
	media: string[];
	storage: string[];
	scroll: string[];
	focus: { desc: string; connected: boolean }[];
};

type QueueSnapshot = {
	ids: string[];
	currentId: string | null;
	nowPlaying: string;
	position: string;
};

function row(page: Page, id: string): Locator {
	return page.locator(`[data-testid="library-item"][data-id="${id}"]`);
}

function rowPrimary(page: Page, id: string): Locator {
	return row(page, id).locator('button.nt-library-row');
}

function rowCheckbox(page: Page, id: string): Locator {
	return row(page, id).getByRole('checkbox');
}

function revealAction(page: Page): Locator {
	return page.getByRole('button', { name: REVEAL_NAME, exact: true });
}

function revealFeedback(page: Page): Locator {
	return page.getByTestId('library-reveal-feedback');
}

function findInput(page: Page): Locator {
	return page.getByRole('searchbox', { name: 'Find in loaded tracks' });
}

function showSelect(page: Page): Locator {
	return page.getByRole('combobox', { name: 'Show' });
}

function orderSelect(page: Page): Locator {
	return page.getByRole('combobox', { name: 'Order tracks' });
}

/** Mutable /api/library mock plus an API request ledger. */
async function routeLibrary(
	page: Page,
	reply: LibraryReply,
	ledger: string[],
	errors: string[]
): Promise<void> {
	page.on('pageerror', (error) => errors.push(error.message));
	page.on('request', (request) => {
		const url = new URL(request.url());
		if (url.pathname.startsWith('/api/') || url.pathname.startsWith('/v1/')) {
			ledger.push(`${request.method()} ${url.pathname}`);
		}
	});
	await page.route('**/api/library', async (route) => {
		if (reply.hold) await reply.hold;
		if (reply.status !== 200) {
			return route.fulfill({
				status: reply.status,
				contentType: 'application/json',
				body: JSON.stringify({ error: 'denied' })
			});
		}
		return route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(reply.items)
		});
	});
}

/**
 * Seed favorites once per browser context (guarded so a reload cannot
 * false-pass after a broken save or clear). Register before installProbes
 * so seed writes stay out of the storage probe.
 */
async function seedFavorites(page: Page, ids: string[]): Promise<void> {
	await page.addInitScript(
		({ authKey, key, initial, account }) => {
			if (window.sessionStorage.getItem('test.libreveal.seeded') !== 'yes') {
				window.sessionStorage.setItem(authKey, account);
				window.localStorage.setItem(key, JSON.stringify(initial));
				window.sessionStorage.setItem('test.libreveal.seeded', 'yes');
			}
		},
		{ authKey: FAVORITES_AUTH_KEY, key: FAVORITES_KEY, initial: ids, account: FAVORITES_ACCOUNT }
	);
}

async function readFavorites(page: Page): Promise<string | null> {
	return page.evaluate((key) => window.localStorage.getItem(key), FAVORITES_KEY);
}

/** Media/storage/scroll/focus probes. Reset after setup; assert deltas. */
async function installProbes(page: Page): Promise<void> {
	await page.addInitScript(() => {
		const store: ProbeSnapshot = { media: [], storage: [], scroll: [], focus: [] };
		(window as unknown as { __libreveal: ProbeSnapshot }).__libreveal = store;
		const describe = (target: EventTarget | null): string => {
			if (!(target instanceof Element)) return '(non-element)';
			const testid = target.getAttribute('data-testid');
			const key =
				target.getAttribute('data-id') ?? target.getAttribute('data-track-id') ?? target.id;
			const rowAction = target.classList.contains('nt-library-row') ? '.nt-library-row' : '';
			const text = (target.textContent ?? '').trim().replace(/\s+/g, ' ').slice(0, 48);
			return `${target.tagName.toLowerCase()}${testid ? `#${testid}` : ''}${key ? `[${key}]` : ''}${rowAction}<${text}>`;
		};
		const mediaProto = HTMLMediaElement.prototype as unknown as Record<
			string,
			(...args: never[]) => unknown
		>;
		for (const method of ['play', 'pause', 'load']) {
			const original = mediaProto[method];
			Object.defineProperty(HTMLMediaElement.prototype, method, {
				configurable: true,
				value: function (this: HTMLMediaElement, ...args: never[]) {
					store.media.push(method);
					return original.apply(this, args);
				}
			});
		}
		const storageProto = Storage.prototype as unknown as Record<
			string,
			(...args: never[]) => unknown
		>;
		for (const method of ['setItem', 'removeItem', 'clear']) {
			const original = storageProto[method];
			Object.defineProperty(Storage.prototype, method, {
				configurable: true,
				value: function (this: Storage, ...args: never[]) {
					store.storage.push(args.length > 0 ? `${method}:${String(args[0])}` : method);
					return original.apply(this, args);
				}
			});
		}
		const elementProto = Element.prototype as unknown as Record<
			string,
			(...args: never[]) => unknown
		>;
		for (const method of ['scrollIntoView', 'scroll', 'scrollTo']) {
			const original = elementProto[method];
			Object.defineProperty(Element.prototype, method, {
				configurable: true,
				value: function (this: Element, ...args: never[]) {
					store.scroll.push(`${method}:${JSON.stringify(args)}`);
					return original.apply(this, args);
				}
			});
		}
		const win = window as unknown as Record<string, (...args: never[]) => unknown>;
		for (const method of ['scroll', 'scrollTo']) {
			const original = win[method].bind(window);
			win[method] = (...args: never[]) => {
				store.scroll.push(`window.${method}:${JSON.stringify(args)}`);
				return original(...args);
			};
		}
		document.addEventListener(
			'focusin',
			(event) => {
				const target = event.target;
				store.focus.push({
					desc: describe(target),
					connected: target instanceof Element ? target.isConnected : false
				});
			},
			true
		);
	});
}

async function readProbes(page: Page): Promise<ProbeSnapshot> {
	return page.evaluate(() => {
		const store = (window as unknown as { __libreveal: ProbeSnapshot }).__libreveal;
		return {
			media: [...store.media],
			storage: [...store.storage],
			scroll: [...store.scroll],
			focus: store.focus.map((entry) => ({ ...entry }))
		};
	});
}

async function resetProbes(page: Page): Promise<void> {
	await page.evaluate(() => {
		const store = (window as unknown as { __libreveal: ProbeSnapshot }).__libreveal;
		store.media.length = 0;
		store.storage.length = 0;
		store.scroll.length = 0;
		store.focus.length = 0;
	});
}

async function openLibrary(page: Page, count: number): Promise<void> {
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(count);
}

/** Append ids in order via the real row handler; first appended is current. */
async function seedQueue(page: Page, ids: string[]): Promise<void> {
	for (const id of ids) {
		await rowPrimary(page, id).click();
	}
	await expect(page.getByTestId('queue-item')).toHaveCount(ids.length);
}

async function queueSnapshot(page: Page): Promise<QueueSnapshot> {
	const ids = await page.getByTestId('queue-item').evaluateAll((nodes) =>
		nodes.map((node) => node.getAttribute('data-track-id') ?? '')
	);
	const current = page.locator('[data-testid="queue-item"][aria-current="true"]');
	const currentCount = await current.count();
	return {
		ids,
		currentId: currentCount > 0 ? await current.first().getAttribute('data-track-id') : null,
		nowPlaying: (await page.getByTestId('now-playing').textContent()) ?? '',
		position: (await page.getByTestId('queue-position').textContent()) ?? ''
	};
}

async function activeDesc(page: Page): Promise<string> {
	return page.evaluate(() => {
		const active = document.activeElement;
		if (!(active instanceof Element)) return '(none)';
		const testid = active.getAttribute('data-testid');
		return `${active.tagName.toLowerCase()}${testid ? `#${testid}` : ''}<${(active.textContent ?? '').trim().replace(/\s+/g, ' ').slice(0, 48)}>`;
	});
}

async function blurActive(page: Page): Promise<void> {
	await page.evaluate(() => {
		if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
	});
}

/** Scroll into view through the patched API, then let the caller reset. */
async function ensureInView(locator: Locator): Promise<void> {
	await locator.evaluate((node) => node.scrollIntoView());
}

function newHarness(items: unknown) {
	return {
		reply: { items, status: 200, hold: null } as LibraryReply,
		ledger: [] as string[],
		errors: [] as string[]
	};
}

test.beforeEach(async ({ context }) => {
	await context.route('**/*', async (route) => {
		const request = route.request();
		const url = new URL(request.url());
		if (url.hostname !== 'localhost' && url.hostname !== '127.0.0.1') {
			return route.abort('blockedbyclient');
		}
		if (request.resourceType() === 'media') return route.abort('blockedbyclient');
		if (url.pathname.startsWith('/api/') || url.pathname.startsWith('/v1/')) {
			return route.fulfill({
				status: 404,
				contentType: 'application/json',
				body: JSON.stringify({ error: 'unmocked' })
			});
		}
		return route.continue();
	});
});

test('[LIBREVEAL-00] control: library loads, queue seeds, queue-panel reveal unchanged', async ({
	page
}) => {
	const harness = newHarness(ITEMS);
	await routeLibrary(page, harness.reply, harness.ledger, harness.errors);
	await installProbes(page);
	await openLibrary(page, ITEMS.length);

	await seedQueue(page, ['libreveal-ember', 'libreveal-pine', 'libreveal-willow']);
	const before = await queueSnapshot(page);
	expect(before.ids).toEqual(['libreveal-ember', 'libreveal-pine', 'libreveal-willow']);
	expect(before.currentId).toBe('libreveal-ember');
	await expect(page.getByTestId('now-playing')).toContainText('Ember Song');
	await expect(page.getByTestId('queue-position')).toContainText('Track 1 of 3');
	await expect(page.getByTestId('now-playing')).not.toContainText('Track 1 of 3');

	// The existing queue-panel reveal keeps its exact name and behavior.
	const queueReveal = page.getByTestId('queue-reveal-current');
	await expect(queueReveal).toHaveAccessibleName('Show current track');
	await expect(page.getByRole('button', { name: 'Show current track', exact: true })).toHaveCount(1);
	await resetProbes(page);
	const callsBefore = [...harness.ledger];
	await queueReveal.click();
	const currentRow = page.locator('[data-testid="queue-item"][aria-current="true"]');
	await expect(currentRow).toBeFocused();
	expect(await queueSnapshot(page)).toEqual(before);
	expect(harness.ledger).toEqual(callsBefore);
	const probes = await readProbes(page);
	expect(probes.media).toEqual([]);
	expect(probes.storage).toEqual([]);
	expect(probes.focus.every((entry) => entry.connected)).toBe(true);
	expect(harness.errors).toEqual([]);
});

test('[LIBREVEAL-01] visible current row: marked, focused, scrolled, everything preserved', async ({
	page
}) => {
	const harness = newHarness(ITEMS);
	await routeLibrary(page, harness.reply, harness.ledger, harness.errors);
	await installProbes(page);
	await openLibrary(page, ITEMS.length);
	await seedQueue(page, ['libreveal-ember', 'libreveal-pine', 'libreveal-willow']);
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Pine Anthem');

	const reveal = revealAction(page);
	await expect(reveal).toBeVisible();
	await expect(reveal).toBeEnabled();
	await expect(reveal).toHaveAccessibleDescription(/filter/i);
	await expect(page.getByTestId('library-reveal-current')).toHaveAccessibleName(REVEAL_NAME);
	// Name collision check: the queue-panel button still resolves exactly once.
	await expect(page.getByRole('button', { name: 'Show current track', exact: true })).toHaveCount(1);

	const queueBefore = await queueSnapshot(page);
	expect(queueBefore.currentId).toBe('libreveal-pine');
	const queuedBadgesBefore = await page.getByTestId('library-queued-badge').count();
	const selectionBefore = (await page.getByTestId('selection-count').textContent()) ?? '';
	const queueFilterBefore = (await page.getByTestId('queue-filter-count').textContent()) ?? '';
	await resetProbes(page);
	const callsBefore = [...harness.ledger];

	await reveal.click();

	const marker = page.getByTestId('library-current-marker');
	await expect(marker).toHaveCount(1);
	await expect(row(page, 'libreveal-pine').getByTestId('library-current-marker')).toHaveCount(1);
	const markerText = (await marker.first().textContent()) ?? '';
	expect(markerText).toMatch(/current/i);
	expect(markerText).not.toMatch(/now playing/i);
	expect(markerText).not.toMatch(/\bplay(ing|ed|s)?\b/i);
	expect(await marker.first().evaluate((node) => node.tagName)).not.toMatch(/^(BUTTON|A|INPUT)$/);

	await expect(rowPrimary(page, 'libreveal-pine')).toBeFocused();
	await expect(row(page, 'libreveal-pine')).toBeInViewport();
	const feedback = revealFeedback(page);
	await expect(feedback).toHaveAttribute('role', 'status');
	await expect(feedback).toContainText(/current track/i);
	await expect(feedback).toContainText(/show|visible|display/i);

	// One-shot: a second activation replaces, never appends.
	await reveal.click();
	const feedbackText = ((await feedback.textContent()) ?? '').replace(/\s+/g, ' ').trim();
	expect(feedbackText).toMatch(/current track/i);
	expect(feedbackText.match(/current track/gi) ?? []).toHaveLength(1);

	expect(await queueSnapshot(page)).toEqual(queueBefore);
	expect(await page.getByTestId('library-queued-badge').count()).toBe(queuedBadgesBefore);
	expect(await page.getByTestId('selection-count').textContent()).toBe(selectionBefore);
	expect(await page.getByTestId('queue-filter-count').textContent()).toBe(queueFilterBefore);
	await expect(findInput(page)).toHaveValue('');
	await expect(showSelect(page)).toHaveValue('all');
	await expect(orderSelect(page)).toHaveValue('original');
	expect(harness.ledger).toEqual(callsBefore);
	const probes = await readProbes(page);
	expect(probes.media).toEqual([]);
	expect(probes.storage).toEqual([]);
	expect(probes.focus.every((entry) => entry.connected)).toBe(true);
	expect(harness.errors).toEqual([]);
});

test('[LIBREVEAL-02] find-hidden row: only find resets, reset disclosed, order kept', async ({
	page
}) => {
	const harness = newHarness(ITEMS);
	await routeLibrary(page, harness.reply, harness.ledger, harness.errors);
	await installProbes(page);
	await openLibrary(page, ITEMS.length);
	await seedQueue(page, ['libreveal-ember', 'libreveal-pine', 'libreveal-willow']);
	await page.getByTestId('player-next').click();
	await orderSelect(page).selectOption('title-desc');
	await expect(orderSelect(page)).toHaveValue('title-desc');

	await findInput(page).fill('Alder');
	await expect(page.getByTestId('library-item')).toHaveCount(1);
	await expect(row(page, 'libreveal-pine')).toHaveCount(0);

	const queueBefore = await queueSnapshot(page);
	const reveal = revealAction(page);
	await expect(reveal).toBeVisible();
	await resetProbes(page);
	const callsBefore = [...harness.ledger];

	await reveal.click();

	await expect(findInput(page)).toHaveValue('');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(rowPrimary(page, 'libreveal-pine')).toBeFocused();
	await expect(row(page, 'libreveal-pine')).toBeInViewport();
	const feedback = revealFeedback(page);
	await expect(feedback).toContainText(/current track/i);
	await expect(feedback).toContainText(/clear|reset/i);
	await expect(feedback).toContainText(/find/i);

	await expect(orderSelect(page)).toHaveValue('title-desc');
	await expect(showSelect(page)).toHaveValue('all');
	expect(await queueSnapshot(page)).toEqual(queueBefore);
	expect(harness.ledger).toEqual(callsBefore);
	const probes = await readProbes(page);
	expect(probes.media).toEqual([]);
	expect(probes.storage).toEqual([]);
	expect(harness.errors).toEqual([]);
});

test('[LIBREVEAL-03] Favorites-hidden row: only the view resets, find untouched', async ({
	page
}) => {
	const harness = newHarness(ITEMS);
	await seedFavorites(page, ['libreveal-ember', 'libreveal-cedar']);
	await routeLibrary(page, harness.reply, harness.ledger, harness.errors);
	await installProbes(page);
	await openLibrary(page, ITEMS.length);
	await seedQueue(page, ['libreveal-ember', 'libreveal-pine', 'libreveal-willow']);
	await page.getByTestId('player-next').click();

	await showSelect(page).selectOption('favorites');
	await expect(page.getByTestId('library-item')).toHaveCount(2);
	await expect(row(page, 'libreveal-pine')).toHaveCount(0);
	const favoritesBefore = await readFavorites(page);

	const queueBefore = await queueSnapshot(page);
	const reveal = revealAction(page);
	await expect(reveal).toBeVisible();
	await resetProbes(page);
	const callsBefore = [...harness.ledger];

	await reveal.click();

	await expect(showSelect(page)).toHaveValue('all');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(findInput(page)).toHaveValue('');
	await expect(rowPrimary(page, 'libreveal-pine')).toBeFocused();
	const feedback = revealFeedback(page);
	await expect(feedback).toContainText(/current track/i);
	await expect(feedback).toContainText(/clear|reset/i);
	await expect(feedback).toContainText(/favorit/i);

	expect(await readFavorites(page)).toBe(favoritesBefore);
	expect(await queueSnapshot(page)).toEqual(queueBefore);
	expect(harness.ledger).toEqual(callsBefore);
	const probes = await readProbes(page);
	expect(probes.media).toEqual([]);
	expect(probes.storage).toEqual([]);
	expect(harness.errors).toEqual([]);
});

test('[LIBREVEAL-04] find- and Favorites-hidden row: both reset, both disclosed', async ({
	page
}) => {
	const harness = newHarness(ITEMS);
	await seedFavorites(page, ['libreveal-ember', 'libreveal-cedar']);
	await routeLibrary(page, harness.reply, harness.ledger, harness.errors);
	await installProbes(page);
	await openLibrary(page, ITEMS.length);
	await seedQueue(page, ['libreveal-ember', 'libreveal-pine', 'libreveal-willow']);
	await page.getByTestId('player-next').click();

	await showSelect(page).selectOption('favorites');
	await findInput(page).fill('Cedar');
	await expect(page.getByTestId('library-item')).toHaveCount(1);
	await expect(row(page, 'libreveal-pine')).toHaveCount(0);

	const queueBefore = await queueSnapshot(page);
	const reveal = revealAction(page);
	await expect(reveal).toBeVisible();
	await resetProbes(page);
	const callsBefore = [...harness.ledger];

	await reveal.click();

	await expect(findInput(page)).toHaveValue('');
	await expect(showSelect(page)).toHaveValue('all');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(rowPrimary(page, 'libreveal-pine')).toBeFocused();
	const feedback = revealFeedback(page);
	await expect(feedback).toContainText(/current track/i);
	await expect(feedback).toContainText(/clear|reset/i);
	await expect(feedback).toContainText(/find/i);
	await expect(feedback).toContainText(/favorit/i);

	expect(await queueSnapshot(page)).toEqual(queueBefore);
	expect(harness.ledger).toEqual(callsBefore);
	const probes = await readProbes(page);
	expect(probes.media).toEqual([]);
	expect(probes.storage).toEqual([]);
	expect(harness.errors).toEqual([]);
});

test('[LIBREVEAL-05] reordered rows: reveal follows the stable current id', async ({ page }) => {
	const harness = newHarness(ITEMS);
	await routeLibrary(page, harness.reply, harness.ledger, harness.errors);
	await installProbes(page);
	await openLibrary(page, ITEMS.length);
	await seedQueue(page, ['libreveal-ember', 'libreveal-pine', 'libreveal-willow']);
	await page.getByTestId('player-next').click();
	await page
		.getByRole('button', { name: 'Move Pine Anthem to bottom', exact: true })
		.click();
	const moved = await queueSnapshot(page);
	expect(moved.ids).toEqual(['libreveal-ember', 'libreveal-willow', 'libreveal-pine']);
	expect(moved.currentId).toBe('libreveal-pine');
	await orderSelect(page).selectOption('title-desc');

	const queueBefore = await queueSnapshot(page);
	const reveal = revealAction(page);
	await expect(reveal).toBeVisible();
	await resetProbes(page);
	const callsBefore = [...harness.ledger];

	await reveal.click();

	const markerRow = row(page, 'libreveal-pine');
	await expect(markerRow.getByTestId('library-current-marker')).toHaveCount(1);
	await expect(page.getByTestId('library-current-marker')).toHaveCount(1);
	await expect(rowPrimary(page, 'libreveal-pine')).toBeFocused();
	await expect(revealFeedback(page)).toContainText(/current track/i);

	expect(await queueSnapshot(page)).toEqual(queueBefore);
	expect(harness.ledger).toEqual(callsBefore);
	const probes = await readProbes(page);
	expect(probes.media).toEqual([]);
	expect(probes.storage).toEqual([]);
	expect(harness.errors).toEqual([]);
});

test('[LIBREVEAL-06] empty queue: honest unavailable feedback, nothing else moves', async ({
	page
}) => {
	const harness = newHarness(ITEMS);
	await routeLibrary(page, harness.reply, harness.ledger, harness.errors);
	await installProbes(page);
	await openLibrary(page, ITEMS.length);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);

	const reveal = revealAction(page);
	await expect(reveal).toBeVisible();
	await expect(reveal).toBeEnabled();
	await resetProbes(page);
	const callsBefore = [...harness.ledger];

	await reveal.click();

	const feedback = revealFeedback(page);
	await expect(feedback).toHaveAttribute('role', 'status');
	await expect(feedback).toContainText(/no current|queue.*empty|empty|nothing selected/i);
	expect(await activeDesc(page)).toContain('button#library-reveal-current');
	await expect(page.getByTestId('library-current-marker')).toHaveCount(0);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(findInput(page)).toHaveValue('');
	await expect(showSelect(page)).toHaveValue('all');
	expect(harness.ledger).toEqual(callsBefore);
	const probes = await readProbes(page);
	expect(probes.media).toEqual([]);
	expect(probes.storage).toEqual([]);
	expect(probes.focus.every((entry) => entry.connected)).toBe(true);
	expect(harness.errors).toEqual([]);
});

test('[LIBREVEAL-07] pruned current: reveal follows the surviving current id', async ({
	page
}) => {
	const harness = newHarness(ITEMS);
	await routeLibrary(page, harness.reply, harness.ledger, harness.errors);
	await installProbes(page);
	await openLibrary(page, ITEMS.length);
	await seedQueue(page, ['libreveal-ember', 'libreveal-pine', 'libreveal-willow']);
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Pine Anthem');

	// Refresh prunes the current track from the snapshot; the shipped
	// reconcile drops it from the queue and the survivor becomes current.
	harness.reply.items = ITEMS.filter((item) => item.id !== 'libreveal-pine');
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length - 1);
	await expect(row(page, 'libreveal-pine')).toHaveCount(0);
	const queueBefore = await queueSnapshot(page);
	expect(queueBefore.ids).toEqual(['libreveal-ember', 'libreveal-willow']);
	expect(queueBefore.currentId).toBe('libreveal-ember');

	const reveal = revealAction(page);
	await expect(reveal).toBeVisible();
	await resetProbes(page);
	const callsBefore = [...harness.ledger];

	await reveal.click();

	// Reveal reads the live current id: ember is marked, never the pruned pine.
	await expect(page.getByTestId('library-current-marker')).toHaveCount(1);
	await expect(row(page, 'libreveal-ember').getByTestId('library-current-marker')).toHaveCount(1);
	await expect(rowPrimary(page, 'libreveal-ember')).toBeFocused();
	const feedback = revealFeedback(page);
	await expect(feedback).toContainText(/current track/i);
	await expect(feedback).toContainText(/show|visible|display/i);
	await expect(feedback).not.toContainText(/not in|no longer|unavailable/i);
	expect(await queueSnapshot(page)).toEqual(queueBefore);
	expect(harness.ledger).toEqual(callsBefore);
	const probes = await readProbes(page);
	expect(probes.media).toEqual([]);
	expect(probes.storage).toEqual([]);
	expect(probes.focus.every((entry) => entry.connected)).toBe(true);
	expect(harness.errors).toEqual([]);
});

test('[LIBREVEAL-08] pending subset survives reveal across a filter reset', async ({ page }) => {
	const harness = newHarness(ITEMS);
	await routeLibrary(page, harness.reply, harness.ledger, harness.errors);
	await installProbes(page);
	await openLibrary(page, ITEMS.length);
	await seedQueue(page, ['libreveal-ember', 'libreveal-pine', 'libreveal-willow']);
	await page.getByTestId('player-next').click();

	await rowCheckbox(page, 'libreveal-ember').check();
	await rowCheckbox(page, 'libreveal-alder').check();
	const selectionBefore = (await page.getByTestId('selection-count').textContent()) ?? '';
	await findInput(page).fill('Alder');
	await expect(row(page, 'libreveal-pine')).toHaveCount(0);

	const queueBefore = await queueSnapshot(page);
	const queuedBadgesBefore = await page.getByTestId('library-queued-badge').count();
	const reveal = revealAction(page);
	await expect(reveal).toBeVisible();
	await resetProbes(page);
	const callsBefore = [...harness.ledger];

	await reveal.click();

	await expect(findInput(page)).toHaveValue('');
	await expect(rowPrimary(page, 'libreveal-pine')).toBeFocused();
	await expect(rowCheckbox(page, 'libreveal-ember')).toBeChecked();
	await expect(rowCheckbox(page, 'libreveal-alder')).toBeChecked();
	expect(await page.getByTestId('selection-count').textContent()).toBe(selectionBefore);
	expect(await page.getByTestId('library-queued-badge').count()).toBe(queuedBadgesBefore);
	expect(await queueSnapshot(page)).toEqual(queueBefore);
	expect(harness.ledger).toEqual(callsBefore);
	const probes = await readProbes(page);
	expect(probes.media).toEqual([]);
	expect(probes.storage).toEqual([]);
	expect(harness.errors).toEqual([]);
});

test('[LIBREVEAL-09] row disappears around the deferred focus: honest, no stale focus', async ({
	page
}) => {
	const harness = newHarness(ITEMS);
	await routeLibrary(page, harness.reply, harness.ledger, harness.errors);
	await installProbes(page);
	await openLibrary(page, ITEMS.length);
	await seedQueue(page, ['libreveal-ember', 'libreveal-pine', 'libreveal-willow']);
	await page.getByTestId('player-next').click();

	// Hold the refresh, reveal against the present row, then drop the track.
	let release!: () => void;
	harness.reply.hold = new Promise<void>((resolve) => {
		release = resolve;
	});
	harness.reply.items = ITEMS.filter((item) => item.id !== 'libreveal-pine');
	const refresh = page.getByRole('button', { name: 'Refresh', exact: true });
	await ensureInView(refresh);
	const reveal = revealAction(page);
	await expect(reveal).toBeVisible();
	await resetProbes(page);
	const callsBefore = harness.ledger.length;
	await refresh.click();
	await reveal.click();
	release();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length - 1);
	await expect(row(page, 'libreveal-pine')).toHaveCount(0);

	// Exactly one refresh fetch happened; reveal never retries a gone row.
	expect(harness.ledger.length - callsBefore).toBe(1);
	await expect(page.getByTestId('library-current-marker')).toHaveCount(0);
	const feedback = revealFeedback(page);
	await expect(feedback).toHaveCount(1);
	const feedbackText = ((await feedback.textContent()) ?? '').replace(/\s+/g, ' ').trim();
	expect(feedbackText.length).toBeGreaterThan(0);
	// Either order is honest: revealed before the drop, or unavailable after it.
	expect(feedbackText).toMatch(/current track|no longer|not in|unavailable|no current/i);
	const probes = await readProbes(page);
	expect(probes.focus.every((entry) => entry.connected)).toBe(true);
	expect(probes.focus.filter((entry) => entry.desc.includes('.nt-library-row'))).toEqual([]);
	expect(probes.media).toEqual([]);
	expect(probes.storage).toEqual([]);
	expect(harness.errors).toEqual([]);
});

test('[LIBREVEAL-10] auth teardown clears state; recovery reveals honestly', async ({ page }) => {
	const harness = newHarness(ITEMS);
	await routeLibrary(page, harness.reply, harness.ledger, harness.errors);
	await installProbes(page);
	await openLibrary(page, ITEMS.length);
	await seedQueue(page, ['libreveal-ember', 'libreveal-pine']);

	harness.reply.status = 401;
	const callsBeforeTeardown = harness.ledger.length;
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.getByRole('alert')).toBeVisible();
	await expect(page.getByTestId('library-current-marker')).toHaveCount(0);
	expect(harness.ledger.length - callsBeforeTeardown).toBe(1);
	const teardownActive = await activeDesc(page);
	expect(teardownActive).not.toContain('.nt-library-row');

	// Next account recovers with its own list and an empty queue.
	harness.reply.status = 200;
	harness.reply.items = [{ id: 'libreveal-next-1', title: 'Next Account Song', artist: 'Far Hill' }];
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(1);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	const reveal = revealAction(page);
	await expect(reveal).toBeVisible();
	await resetProbes(page);
	const callsBefore = [...harness.ledger];

	await reveal.click();

	await expect(revealFeedback(page)).toContainText(/no current|queue.*empty|empty|nothing selected/i);
	await expect(page.getByTestId('library-current-marker')).toHaveCount(0);
	expect(harness.ledger).toEqual(callsBefore);
	const probes = await readProbes(page);
	expect(probes.media).toEqual([]);
	expect(probes.storage).toEqual([]);
	expect(probes.focus.every((entry) => entry.connected)).toBe(true);
	expect(harness.errors).toEqual([]);
});

test('[LIBREVEAL-11] reduced motion: focus lands, scrolling never smooth', async ({ page }) => {
	const harness = newHarness(MANY);
	await routeLibrary(page, harness.reply, harness.ledger, harness.errors);
	await installProbes(page);
	await page.emulateMedia({ reducedMotion: 'reduce' });
	expect(await page.evaluate(() => matchMedia('(prefers-reduced-motion: reduce)').matches)).toBe(
		true
	);
	await openLibrary(page, MANY.length);
	const currentId = 'libreveal-ridge-37';
	await seedQueue(page, ['libreveal-ridge-36', currentId, 'libreveal-ridge-38']);
	await page.getByTestId('player-next').click();
	// Seeding scrolled the tall list; reset to the top so the row is below the fold.
	await page.evaluate(() => window.scrollTo(0, 0));
	await expect(row(page, currentId)).not.toBeInViewport();

	const reveal = revealAction(page);
	await expect(reveal).toBeVisible();
	await ensureInView(reveal);
	await resetProbes(page);

	await reveal.click();

	await expect(rowPrimary(page, currentId)).toBeFocused();
	await expect(row(page, currentId)).toBeInViewport();
	await expect(revealFeedback(page)).toContainText(/current track/i);
	const probes = await readProbes(page);
	expect(probes.scroll.filter((call) => call.includes('smooth'))).toEqual([]);
	expect(probes.media).toEqual([]);
	expect(harness.errors).toEqual([]);
});

test('[LIBREVEAL-12] no automatic focus or scrolling on next, refresh or mount', async ({
	page
}) => {
	const harness = newHarness(ITEMS);
	await routeLibrary(page, harness.reply, harness.ledger, harness.errors);
	await installProbes(page);
	await openLibrary(page, ITEMS.length);
	await seedQueue(page, ['libreveal-ember', 'libreveal-pine', 'libreveal-willow']);

	const next = page.getByTestId('player-next');
	await ensureInView(next);
	await blurActive(page);
	expect(await activeDesc(page)).toContain('body');
	await resetProbes(page);
	await next.click();
	await expect(page.getByTestId('now-playing')).toContainText('Pine Anthem');
	let probes = await readProbes(page);
	expect(probes.focus.filter((entry) => entry.desc.includes('.nt-library-row'))).toEqual([]);
	expect(probes.scroll).toEqual([]);
	expect(await activeDesc(page)).not.toContain('.nt-library-row');

	const refresh = page.getByRole('button', { name: 'Refresh', exact: true });
	await ensureInView(refresh);
	await blurActive(page);
	await resetProbes(page);
	await refresh.click();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	probes = await readProbes(page);
	expect(probes.focus.filter((entry) => entry.desc.includes('.nt-library-row'))).toEqual([]);
	expect(probes.scroll).toEqual([]);
	expect(await activeDesc(page)).not.toContain('.nt-library-row');

	// Mount with a below-fold current track via a share link: still no auto reveal.
	// (Framework navigation settles at the top, so window-level scrolls are
	// ignored here; a row reveal would scroll down or focus the row action.)
	harness.reply.items = MANY;
	const mountedId = 'libreveal-ridge-37';
	await page.goto(`/library?track=${mountedId}&t=45`);
	await expect(page.getByTestId('library-item')).toHaveCount(MANY.length);
	await expect(page.getByTestId('now-playing')).toContainText('Ridge Song');
	await expect(row(page, mountedId)).not.toBeInViewport();
	probes = await readProbes(page);
	expect(probes.focus.filter((entry) => entry.desc.includes('.nt-library-row'))).toEqual([]);
	expect(
		probes.scroll.filter(
			(call) =>
				call.startsWith('scrollIntoView:') ||
				call.startsWith('scroll:') ||
				call.startsWith('scrollTo:')
		)
	).toEqual([]);
	expect(await page.evaluate(() => window.scrollY)).toBe(0);
	expect(await activeDesc(page)).toContain('body');
	const feedbackCount = await revealFeedback(page).count();
	if (feedbackCount > 0) {
		expect(((await revealFeedback(page).textContent()) ?? '').trim()).toBe('');
	}
	expect(harness.errors).toEqual([]);
});

test('[LIBREVEAL-13] keyboard activation moves visible focus to the row action', async ({
	page
}) => {
	const harness = newHarness(ITEMS);
	await routeLibrary(page, harness.reply, harness.ledger, harness.errors);
	await installProbes(page);
	await openLibrary(page, ITEMS.length);
	await seedQueue(page, ['libreveal-ember', 'libreveal-pine', 'libreveal-willow']);
	await page.getByTestId('player-next').click();

	const reveal = revealAction(page);
	await expect(reveal).toBeVisible();
	expect(await reveal.evaluate((node) => (node as HTMLElement).tabIndex)).toBeGreaterThanOrEqual(0);
	await resetProbes(page);

	await reveal.focus();
	await page.keyboard.press('Enter');

	const primary = rowPrimary(page, 'libreveal-pine');
	await expect(primary).toBeFocused();
	const ring = await primary.evaluate((node) => {
		const style = getComputedStyle(node);
		return { outline: style.outlineStyle, shadow: style.boxShadow };
	});
	expect(ring.outline !== 'none' || ring.shadow !== 'none').toBe(true);
	await expect(revealFeedback(page)).toContainText(/current track/i);
	const probes = await readProbes(page);
	expect(probes.focus.every((entry) => entry.connected)).toBe(true);
	expect(harness.errors).toEqual([]);
});

for (const scheme of ['light', 'dark'] as const) {
	test.describe(`narrow ${scheme}`, () => {
		test.use({ viewport: { width: 390, height: 844 }, hasTouch: true, colorScheme: scheme });

		test(`[LIBREVEAL-14] narrow ${scheme} touch: reveal stays usable`, async ({ page }) => {
			const harness = newHarness(ITEMS);
			await routeLibrary(page, harness.reply, harness.ledger, harness.errors);
			await installProbes(page);
			await openLibrary(page, ITEMS.length);
			await seedQueue(page, ['libreveal-ember', 'libreveal-pine']);
			await page.getByTestId('player-next').click();

			const reveal = revealAction(page);
			await expect(reveal).toBeVisible();
			await expect(reveal).toBeEnabled();
			await resetProbes(page);

			await reveal.tap();

			await expect(rowPrimary(page, 'libreveal-pine')).toBeFocused();
			await expect(row(page, 'libreveal-pine')).toBeInViewport();
			await expect(revealFeedback(page)).toContainText(/current track/i);
			const overflow = await page.evaluate(
				() => document.documentElement.scrollWidth - window.innerWidth
			);
			expect(overflow).toBeLessThanOrEqual(1);
			expect(harness.errors).toEqual([]);
		});
	});
}

test('[LIBREVEAL-15] Selected-only-hidden row: view resets, subset ids kept', async ({
	page
}) => {
	const harness = newHarness(ITEMS);
	await routeLibrary(page, harness.reply, harness.ledger, harness.errors);
	await installProbes(page);
	await openLibrary(page, ITEMS.length);
	await seedQueue(page, ['libreveal-ember', 'libreveal-pine']);
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Pine Anthem');

	await rowCheckbox(page, 'libreveal-ember').check();
	const selectionBefore = (await page.getByTestId('selection-count').textContent()) ?? '';
	await showSelect(page).selectOption('selected');
	await expect(page.getByTestId('library-item')).toHaveCount(1);
	await expect(row(page, 'libreveal-pine')).toHaveCount(0);

	const queueBefore = await queueSnapshot(page);
	const reveal = revealAction(page);
	await expect(reveal).toBeVisible();
	await resetProbes(page);
	const callsBefore = [...harness.ledger];

	await reveal.click();

	await expect(showSelect(page)).toHaveValue('all');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(rowPrimary(page, 'libreveal-pine')).toBeFocused();
	await expect(rowCheckbox(page, 'libreveal-ember')).toBeChecked();
	expect(await page.getByTestId('selection-count').textContent()).toBe(selectionBefore);
	const feedback = revealFeedback(page);
	await expect(feedback).toContainText(/current track/i);
	await expect(feedback).toContainText(/clear|reset/i);
	await expect(feedback).toContainText(/select/i);

	expect(await queueSnapshot(page)).toEqual(queueBefore);
	expect(harness.ledger).toEqual(callsBefore);
	const probes = await readProbes(page);
	expect(probes.media).toEqual([]);
	expect(probes.storage).toEqual([]);
	expect(harness.errors).toEqual([]);
});
