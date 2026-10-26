import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Acceptance-owned: the paired build makes this file pass without editing
 * it. Offline browser contract for browsing the already-loaded library
 * tracks grouped by artist, not implementation/source-shape pins. The
 * existing library-local-filter, library-find-scope, library-ordering,
 * library-favorites-management, library-selection and library-subset-append
 * banks stay protected and unedited, and every behavior they pin must keep
 * passing with grouping off.
 *
 * UI contract the build must provide:
 * - A toggle whose accessible name is exactly "Group by artist", placed
 *   with the existing Find / Order tracks / Show list controls above the
 *   loaded-track list. Shape is the build's choice among native,
 *   keyboard/touch-operable controls: a native checkbox, a native
 *   `role="switch"` control, or a toggle <button> carrying
 *   `aria-pressed`. The on/off state must always be programmatically
 *   readable from the control (checked / aria-checked / aria-pressed),
 *   never color alone. Default is off: the shipped flat list.
 * - Grouping is a pure view transform over the SAME displayed-items
 *   pipeline as find (text + scope), ordering and the All tracks /
 *   Favorites only view: sections reflect only tracks currently
 *   displayed, and an artist with no displayed track renders no section.
 * - Grouping key and comparator (stated here so the build matches it):
 *   the key is the track's display artist (non-blank string artist, as in
 *   the shipped display rules) trimmed and folded with
 *   `String.toLowerCase()`. Case/whitespace variants of one artist share
 *   ONE section; accents are never stripped, so "echo" and "écho"-style
 *   keys stay distinct. Named sections sort ascending by that key with
 *   the locale-independent UTF-16 code-unit comparator
 *   `a < b ? -1 : a > b ? 1 : 0` — never `localeCompare` — so a key
 *   starting with "é" (U+00E9) sorts AFTER "z". Tracks with a missing,
 *   blank or non-string artist form one last section labelled exactly
 *   "Unknown artist" (unnamed last, as in the shipped ordering rules);
 *   they are never dropped and never given invented artist metadata on
 *   their rows. Section order is always that ascending order, whatever
 *   the Order tracks choice is; the ordering choice decides only the
 *   order of tracks INSIDE each section (tracks keep their relative
 *   order in the currently displayed list — a stable partition, no
 *   re-sort beyond the existing pipeline).
 * - Each section is introduced by a real heading element (h2–h6, below
 *   the page h1) whose whole text is the section label: one of the
 *   section's actual artist spellings, trimmed (casing is the build's
 *   choice among those spellings; this bank compares labels
 *   case-insensitively against the grouping key), or "Unknown artist".
 *   Headings carry no counts or other decoration in their text and are
 *   not tab stops. Every grouped row stays a list item inside a list.
 * - Grouped rows keep the flat row's controls and accessible names
 *   unchanged (Select <title> checkbox, the main row button, Copy link
 *   <title>, Star/Unstar <title>), in the same per-row keyboard order.
 *   Row actions act on the exact track clicked; this bank always
 *   addresses them by exact accessible name, never substring, because
 *   "Harbor" is a substring of "Harbor Song" in the fixture.
 * - Toggling grouping on or off mutates nothing else: no fetch, no
 *   queue/current/media change, no favorites, selection, find text/scope,
 *   ordering or view change, and no storage write (the choice is
 *   visit-local state with no new storage key, like the find query: a
 *   reload and an account/session teardown return to the flat list).
 *   Turning grouping off restores the flat list exactly — same rows,
 *   same order as the same pipeline state with grouping never enabled.
 * - Feedback stays polite and scoped to <main>: toggling adds no
 *   assertive live region and raises no alert, and operating the toggle
 *   never dumps focus to <body>. The toggle and every grouped row
 *   control keep targets of at least 24x24 CSS px.
 *
 * Selection note: the shipped Show control offers only "All tracks" and
 * "Favorites only" at this bank's base — there is no selected-only view
 * to compose with. Selection composition is therefore pinned through the
 * shipped selection surface (row checkboxes, selection count,
 * hidden-selection count and Append selected) under grouping. Inventing
 * a new view is out of scope for the paired build.
 *
 * Fixture discipline: visible title/artist text is digit-free and avoids
 * the words "star"/"favorite"/"group", so count, star and toggle lookups
 * can only match their own controls. grp-echo-a "Harbor" and grp-echo-b
 * "Harbor Song" are the substring pair; "Echo"/"echo" and
 * "beta Band"/"Beta Band"/"zulu Band"/"ZULU BAND" are case variants of
 * one artist each; grp-accent's "éclair Ensemble" pins the code-unit
 * comparator; grp-orphan has no artist and grp-blank a blank one.
 *
 * Case IDs (stable): GA-B00 control (passes on the baseline), GA-B01
 * toggle bundle + full partition + Unknown truthfulness, GA-B02 ordering
 * composition (title asc/desc, artist desc), GA-B03 find composition
 * (both + artist scope + zero match), GA-B04 Favorites-view composition,
 * GA-B05 selection composition + Append selected, GA-B06 exact-track row
 * actions (substring trap), GA-B07 toggle-off restoration + no mutation,
 * GA-B08 headings/list semantics, keyboard order, polite feedback and
 * target sizes, GA-B09 320px light/dark + touch, GA-B10 reload reset,
 * GA-B11 account-teardown reset.
 *
 * Focused command (repo root, deterministic, offline):
 *   CI=1 PUBLIC_API_BASE_URL='' pnpm --dir apps/web exec playwright test tests/library-group-by-artist.spec.ts --workers=1 --retries=0
 */

const ITEMS = [
	{ id: 'grp-zulu-a', title: 'Zebra Song', artist: 'zulu Band' },
	{ id: 'grp-echo-a', title: 'Harbor', artist: 'Echo' },
	{ id: 'grp-orphan', title: 'Orphan Song' },
	{ id: 'grp-beta-a', title: 'Beta Morning', artist: 'beta Band' },
	{ id: 'grp-echo-b', title: 'Harbor Song', artist: 'echo' },
	{ id: 'grp-accent', title: 'Acclaim Song', artist: 'éclair Ensemble' },
	{ id: 'grp-blank', title: 'Quiet Song', artist: '   ' },
	{ id: 'grp-beta-b', title: 'Beta Evening', artist: 'Beta Band' },
	{ id: 'grp-alpha', title: 'Alpha Song', artist: 'Alpha' },
	{ id: 'grp-zulu-b', title: 'Aardvark Song', artist: 'ZULU BAND' }
];
const API_IDS = ITEMS.map((item) => item.id);

/** Grouping keys in the bank's stated ascending order (Unknown last). */
const SECTION_KEYS = [
	'alpha',
	'beta band',
	'echo',
	'zulu band',
	'éclair ensemble',
	'unknown artist'
] as const;

/** Full partition under Original order (stable partition of API order). */
const GROUPED_ORIGINAL: [string, string[]][] = [
	['alpha', ['grp-alpha']],
	['beta band', ['grp-beta-a', 'grp-beta-b']],
	['echo', ['grp-echo-a', 'grp-echo-b']],
	['zulu band', ['grp-zulu-a', 'grp-zulu-b']],
	['éclair ensemble', ['grp-accent']],
	['unknown artist', ['grp-orphan', 'grp-blank']]
];

const NEXT_ACCOUNT = [
	{ id: 'next-zeta', title: 'Next Zeta Song', artist: 'Zeta Next' },
	{ id: 'next-alpha', title: 'Next Alpha Song', artist: 'Alpha Next' }
];
const NEXT_KEYS = ['alpha next', 'zeta next', 'unknown artist'];

type LibraryReply = { items: unknown; status?: number };

async function mockLibrary(page: Page, reply: LibraryReply = { items: ITEMS }) {
	let requests = 0;
	await page.route('**/api/library', (route) => {
		requests += 1;
		return route.fulfill({
			status: reply.status ?? 200,
			contentType: 'application/json',
			body: JSON.stringify(reply.items)
		});
	});
	return { requests: () => requests };
}

async function openLibrary(page: Page) {
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expectRows(page, API_IDS);
}

async function expectRows(page: Page, ids: string[]) {
	await expect
		.poll(() =>
			page
				.getByTestId('library-item')
				.evaluateAll((rows) => rows.map((row) => row.getAttribute('data-id')))
		)
		.toEqual(ids);
}

function row(page: Page, id: string): Locator {
	return page.locator(`[data-testid="library-item"][data-id="${id}"]`);
}

function findField(page: Page): Locator {
	return page
		.getByRole('searchbox', { name: /find|search/i })
		.or(page.getByRole('textbox', { name: /find|search/i }));
}

async function requireFind(page: Page): Promise<Locator> {
	const field = findField(page);
	await expect(field, 'library must expose the labeled find-in-loaded-tracks field').toBeVisible();
	return field;
}

function ordering(page: Page): Locator {
	return page.getByRole('combobox', { name: /order|sort/i });
}

function findScope(page: Page): Locator {
	return page.getByRole('combobox', {
		name: /search in|find in|search by|find by|find scope|search scope|field|scope/i
	});
}

function view(page: Page): Locator {
	return page.getByRole('combobox', { name: 'Show', exact: true });
}

function starButton(page: Page, id: string): Locator {
	return row(page, id).getByRole('button', { name: /star|favorite/i });
}

async function favoriteShelfIds(page: Page): Promise<(string | null)[]> {
	return page
		.getByTestId('favorite-item')
		.evaluateAll((items) => items.map((item) => item.getAttribute('data-id')));
}

/* ---- the Group by artist toggle: one of three sanctioned shapes ---- */

type GroupKind = 'checkbox' | 'switch' | 'button';

function groupToggle(page: Page, kind: GroupKind): Locator {
	if (kind === 'checkbox') {
		return page.getByRole('checkbox', { name: 'Group by artist', exact: true });
	}
	if (kind === 'switch') {
		return page.getByRole('switch', { name: 'Group by artist', exact: true });
	}
	return page.getByRole('button', { name: 'Group by artist', exact: true });
}

async function groupKind(page: Page): Promise<GroupKind | null> {
	const kinds: GroupKind[] = [];
	for (const kind of ['checkbox', 'switch', 'button'] as const) {
		if ((await groupToggle(page, kind).count()) > 0) kinds.push(kind);
	}
	expect(
		kinds.length,
		'exactly one "Group by artist" toggle must exist (checkbox, switch or aria-pressed button)'
	).toBeLessThanOrEqual(1);
	return kinds[0] ?? null;
}

async function requireGroup(page: Page): Promise<GroupKind> {
	const kind = await groupKind(page);
	expect(
		kind,
		'library must expose a native "Group by artist" toggle (checkbox, switch, or ' +
			'aria-pressed button) with a programmatically readable on/off state'
	).not.toBeNull();
	const toggle = groupToggle(page, kind as GroupKind);
	await expect(toggle, '"Group by artist" toggle must be visible').toBeVisible();
	return kind as GroupKind;
}

async function isGrouped(page: Page, kind: GroupKind): Promise<boolean> {
	const toggle = groupToggle(page, kind);
	if (kind === 'checkbox') return toggle.isChecked();
	const attribute = kind === 'switch' ? 'aria-checked' : 'aria-pressed';
	return (await toggle.getAttribute(attribute)) === 'true';
}

async function expectGrouped(page: Page, kind: GroupKind, on: boolean) {
	await expect
		.poll(() => isGrouped(page, kind), {
			message: `"Group by artist" must read ${on ? 'on' : 'off'}`
		})
		.toBe(on);
}

async function setGrouped(page: Page, kind: GroupKind, on: boolean) {
	if ((await isGrouped(page, kind)) !== on) {
		const toggle = groupToggle(page, kind);
		if (kind === 'checkbox') {
			if (on) await toggle.check();
			else await toggle.uncheck();
		} else {
			await toggle.click();
		}
	}
	await expectGrouped(page, kind, on);
}

/* ---- artist sections, read from DOM order of headings + rows ---- */

type Section = { key: string | null; ids: string[] };

async function readSections(page: Page, keys: readonly string[] = SECTION_KEYS): Promise<Section[]> {
	return page.evaluate((known) => {
		const sections: { key: string | null; ids: string[] }[] = [];
		let current: { key: string | null; ids: string[] } | null = null;
		const nodes = document.querySelectorAll(
			'h1, h2, h3, h4, h5, h6, [data-testid="library-item"]'
		);
		for (const node of nodes) {
			if (node.getAttribute('data-testid') === 'library-item') {
				if (!current) {
					current = { key: null, ids: [] };
					sections.push(current);
				}
				current.ids.push(node.getAttribute('data-id') ?? '');
			} else {
				const text = (node.textContent ?? '').trim().replace(/\s+/g, ' ').toLowerCase();
				if ((known as string[]).includes(text)) {
					current = { key: text, ids: [] };
					sections.push(current);
				}
			}
		}
		return sections;
	}, [...keys]);
}

async function expectSections(
	page: Page,
	expected: [string, string[]][],
	keys: readonly string[] = SECTION_KEYS
) {
	const wanted = expected.map(([key, ids]) => ({ key, ids }));
	await expect
		.poll(() => readSections(page, keys), {
			message: 'grouped sections (labelled, in stated order, stable in-section order)'
		})
		.toEqual(wanted);
	// No track is dropped or duplicated by grouping the displayed set.
	const flat = wanted.flatMap((section) => section.ids);
	expect(new Set(flat).size, 'grouped rows must not duplicate a track').toBe(flat.length);
	await expect(page.getByTestId('library-item')).toHaveCount(flat.length);
}

/** Flat list: exact row order and no artist section heading anywhere. */
async function expectFlat(page: Page, ids: string[], keys: readonly string[] = SECTION_KEYS) {
	await expectRows(page, ids);
	const sections = await readSections(page, keys);
	expect(
		sections.filter((section) => section.key !== null),
		'flat list must render no artist section headings'
	).toEqual([]);
}

async function readStorage(page: Page) {
	return page.evaluate(() => ({
		local: Object.entries(window.localStorage).sort() as [string, string][],
		session: Object.entries(window.sessionStorage).sort() as [string, string][]
	}));
}

async function queueState(page: Page) {
	return page.evaluate(() => {
		const rows = [...document.querySelectorAll('[data-testid="queue-item"]')];
		const current = rows.find((item) => item.getAttribute('aria-current') === 'true');
		return {
			ids: rows.map((item) => item.getAttribute('data-track-id') ?? item.getAttribute('data-id')),
			currentId: current?.getAttribute('data-track-id') ?? current?.getAttribute('data-id') ?? null,
			now: document.querySelector('[data-testid="now-playing"]')?.textContent ?? null,
			media: [...document.querySelectorAll('audio, video')].map((element) => {
				const media = element as HTMLMediaElement;
				return { src: media.currentSrc, paused: media.paused, ended: media.ended };
			}),
			calls: Reflect.get(window, '__groupMediaCalls') ?? null
		};
	});
}

async function expectQueue(page: Page, ids: string[], current: string, title: string) {
	await expect
		.poll(async () => (await queueState(page)).ids)
		.toEqual(ids);
	await expect(page.getByTestId('now-playing')).toContainText(title);
	await expect.poll(async () => (await queueState(page)).currentId).toBe(current);
}

async function authTransition(page: Page) {
	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate(() => {
		localStorage.setItem('nntune.library.auth-transition.v1', 'group-by-artist-test');
		localStorage.removeItem('nntune.library.auth-transition.v1');
	});
	await otherTab.close();
}

test.beforeEach(async ({ context }) => {
	// No catalog, account, media or production network. The specific library
	// page.route mocks below take precedence over this context boundary.
	await context.route('**/*', (route) => {
		const url = new URL(route.request().url());
		if (!['localhost', '127.0.0.1'].includes(url.hostname)) return route.abort('blockedbyclient');
		if (url.pathname.startsWith('/api/')) {
			return route.fulfill({ status: 404, contentType: 'application/json', body: '{}' });
		}
		return route.continue();
	});
});

test('[GA-B00] control: shipped flat list, find and queueing work before grouping lands', async ({
	page
}) => {
	const api = await mockLibrary(page);
	await openLibrary(page);
	await expectFlat(page, API_IDS);
	const field = await requireFind(page);
	await field.fill('beta');
	await expectRows(page, ['grp-beta-a', 'grp-beta-b']);
	await page.getByRole('button', { name: /clear search/i }).first().click();
	await expect(field).toHaveValue('');
	await expectFlat(page, API_IDS);
	await row(page, 'grp-alpha').click();
	await row(page, 'grp-zulu-a').click();
	await expectQueue(page, ['grp-alpha', 'grp-zulu-a'], 'grp-alpha', 'Alpha Song');
	expect(api.requests()).toBe(1);
});

test('[GA-B01] bundle: toggle defaults off; on renders the full deterministic partition with a truthful Unknown artist section', async ({
	page
}) => {
	const api = await mockLibrary(page);
	await openLibrary(page);
	const kind = await requireGroup(page);
	await expectGrouped(page, kind, false);
	await expectFlat(page, API_IDS);

	await setGrouped(page, kind, true);
	// Ascending by trimmed lowercase key, code-unit order (éclair AFTER
	// zulu), case variants merged, Unknown last, in-section API order.
	await expectSections(page, GROUPED_ORIGINAL);

	// Exactly one real heading per section label.
	for (const key of SECTION_KEYS) {
		const headings = page.getByRole('heading', { name: new RegExp(`^${key}$`, 'i') });
		await expect(headings, `one heading for section "${key}"`).toHaveCount(1);
	}

	// Unknown rows carry no invented artist metadata: their main row
	// buttons are named by title alone, exactly as in the flat list, and
	// no row text claims an "Unknown" artist.
	for (const [id, title] of [
		['grp-orphan', 'Orphan Song'],
		['grp-blank', 'Quiet Song']
	] as const) {
		await expect(
			page.getByRole('button', { name: title, exact: true }),
			`"${title}" row button must not gain an invented artist suffix`
		).toHaveCount(1);
		const rowText = await row(page, id).evaluate((element) => element.textContent ?? '');
		expect(rowText, `"${title}" row must not display invented metadata`).not.toMatch(
			/unknown/i
		);
	}

	// Named rows keep their flat accessible names while grouped.
	await expect(
		page.getByRole('checkbox', { name: 'Select Beta Evening', exact: true })
	).toHaveCount(1);
	await expect(
		page.getByRole('button', { name: 'Copy link Beta Evening', exact: true })
	).toHaveCount(1);
	expect(api.requests(), 'grouping must not fetch').toBe(1);
});

test('[GA-B02] ordering composition: sections stay ascending; title/artist asc-desc decide only in-section order', async ({
	page
}) => {
	await mockLibrary(page);
	await openLibrary(page);
	const kind = await requireGroup(page);

	// Title A–Z over the flat list first, so the in-section expectation is
	// visibly the ordering, not API order (Beta Evening before Beta
	// Morning, Aardvark Song before Zebra Song inside their sections).
	await ordering(page).selectOption({ label: 'Title A–Z' });
	await expectFlat(page, [
		'grp-zulu-b',
		'grp-accent',
		'grp-alpha',
		'grp-beta-b',
		'grp-beta-a',
		'grp-echo-a',
		'grp-echo-b',
		'grp-orphan',
		'grp-blank',
		'grp-zulu-a'
	]);
	await setGrouped(page, kind, true);
	await expectSections(page, [
		['alpha', ['grp-alpha']],
		['beta band', ['grp-beta-b', 'grp-beta-a']],
		['echo', ['grp-echo-a', 'grp-echo-b']],
		['zulu band', ['grp-zulu-b', 'grp-zulu-a']],
		['éclair ensemble', ['grp-accent']],
		['unknown artist', ['grp-orphan', 'grp-blank']]
	]);
	await expect(ordering(page).locator('option:checked')).toHaveText('Title A–Z');

	// Title Z–A flips in-section order only; sections remain ascending and
	// the Unknown section's tracks follow the title ordering too.
	await ordering(page).selectOption({ label: 'Title Z–A' });
	await expectSections(page, [
		['alpha', ['grp-alpha']],
		['beta band', ['grp-beta-a', 'grp-beta-b']],
		['echo', ['grp-echo-b', 'grp-echo-a']],
		['zulu band', ['grp-zulu-a', 'grp-zulu-b']],
		['éclair ensemble', ['grp-accent']],
		['unknown artist', ['grp-blank', 'grp-orphan']]
	]);

	// Artist Z–A makes the flat displayed order descend by artist, but
	// grouped sections must STILL be ascending (an implementation that
	// merely chunks consecutive rows would render them descending).
	await setGrouped(page, kind, false);
	await ordering(page).selectOption({ label: 'Artist Z–A' });
	await expectFlat(page, [
		'grp-accent',
		'grp-zulu-a',
		'grp-zulu-b',
		'grp-echo-a',
		'grp-echo-b',
		'grp-beta-a',
		'grp-beta-b',
		'grp-alpha',
		'grp-orphan',
		'grp-blank'
	]);
	await setGrouped(page, kind, true);
	await expectSections(page, GROUPED_ORIGINAL);
	await expect(ordering(page).locator('option:checked')).toHaveText('Artist Z–A');

	// And returning to Original order restores API-relative sections
	// without anyone touching the toggle.
	await ordering(page).selectOption({ label: 'Original order' });
	await expectSections(page, GROUPED_ORIGINAL);
	await expectGrouped(page, kind, true);
});

test('[GA-B03] find composition: sections reflect only displayed tracks (both scope, artist scope, zero match)', async ({
	page
}) => {
	await mockLibrary(page);
	await openLibrary(page);
	const kind = await requireGroup(page);
	const field = await requireFind(page);
	await setGrouped(page, kind, true);
	await expectSections(page, GROUPED_ORIGINAL);

	await field.fill('beta');
	await expectSections(page, [['beta band', ['grp-beta-a', 'grp-beta-b']]]);

	// "song" drops the whole Beta section (no empty section renders) and
	// the title-less second Echo track stays while "Harbor" goes.
	await field.fill('song');
	await expectSections(page, [
		['alpha', ['grp-alpha']],
		['echo', ['grp-echo-b']],
		['zulu band', ['grp-zulu-a', 'grp-zulu-b']],
		['éclair ensemble', ['grp-accent']],
		['unknown artist', ['grp-orphan', 'grp-blank']]
	]);
	await expect(
		page.getByRole('heading', { name: /^beta band$/i }),
		'an artist with no displayed track renders no section'
	).toHaveCount(0);

	// The shipped find scope composes too: Artist only + "echo" keeps the
	// two case-variant Echo rows as the single section.
	await findScope(page).selectOption({ label: 'Artist only' });
	await field.fill('echo');
	await expectSections(page, [['echo', ['grp-echo-a', 'grp-echo-b']]]);
	await findScope(page).selectOption({ label: 'Both title and artist' });

	// Zero matches: no rows and no artist headings, toggle still on and
	// reachable; clearing the find restores the grouped partition intact.
	await field.fill('quixotic');
	await expect(page.getByTestId('library-item')).toHaveCount(0);
	expect(
		(await readSections(page)).filter((section) => section.key !== null),
		'zero displayed tracks must render zero artist sections'
	).toEqual([]);
	await expectGrouped(page, kind, true);
	await page.getByRole('button', { name: /clear search/i }).first().click();
	await expect(field).toHaveValue('');
	await expectSections(page, GROUPED_ORIGINAL);
});

test('[GA-B04] Favorites-view composition: grouped sections intersect Favorites only; unstarring the last track removes its section', async ({
	page
}) => {
	await mockLibrary(page);
	await openLibrary(page);
	const kind = await requireGroup(page);

	await starButton(page, 'grp-echo-a').click();
	await starButton(page, 'grp-beta-b').click();
	await starButton(page, 'grp-blank').click();
	const shelfBefore = await favoriteShelfIds(page);
	expect([...shelfBefore].sort()).toEqual(['grp-beta-b', 'grp-blank', 'grp-echo-a']);

	await view(page).selectOption({ label: 'Favorites only' });
	await expectFlat(page, ['grp-echo-a', 'grp-blank', 'grp-beta-b']);
	await setGrouped(page, kind, true);
	await expectSections(page, [
		['beta band', ['grp-beta-b']],
		['echo', ['grp-echo-a']],
		['unknown artist', ['grp-blank']]
	]);
	expect(await favoriteShelfIds(page), 'grouping must not reorder favorites').toEqual(
		shelfBefore
	);
	await expect(starButton(page, 'grp-beta-b')).toHaveAttribute('aria-pressed', 'true');

	// Unstar the only Beta track from inside the grouped favorites view:
	// the row and its now-empty section both go, the rest stay grouped.
	await starButton(page, 'grp-beta-b').click();
	await expectSections(page, [
		['echo', ['grp-echo-a']],
		['unknown artist', ['grp-blank']]
	]);
	expect([...(await favoriteShelfIds(page))].sort()).toEqual(['grp-blank', 'grp-echo-a']);

	// Back to All tracks the full grouped partition returns, favorites
	// shelf untouched by the view switch or the grouping.
	await view(page).selectOption({ label: 'All tracks' });
	await expectSections(page, GROUPED_ORIGINAL);
	expect([...(await favoriteShelfIds(page))].sort()).toEqual(['grp-blank', 'grp-echo-a']);
});

test('[GA-B05] selection composition: grouped checkboxes, counts and Append selected act on the exact selected tracks', async ({
	page
}) => {
	await mockLibrary(page);
	await openLibrary(page);
	const kind = await requireGroup(page);
	const field = await requireFind(page);
	await setGrouped(page, kind, true);

	const echoBChoice = page.getByRole('checkbox', { name: 'Select Harbor Song', exact: true });
	const betaBChoice = page.getByRole('checkbox', { name: 'Select Beta Evening', exact: true });
	await echoBChoice.check();
	await betaBChoice.check();
	await expect(page.getByTestId('selection-count')).toHaveText('2 selected');
	await expect(page.getByTestId('selection-hidden-count')).toHaveCount(0);

	// Toggling grouping off and back on never clears the selection.
	await setGrouped(page, kind, false);
	await expectFlat(page, API_IDS);
	await expect(echoBChoice).toBeChecked();
	await expect(betaBChoice).toBeChecked();
	await expect(page.getByTestId('selection-count')).toHaveText('2 selected');
	await setGrouped(page, kind, true);
	await expectSections(page, GROUPED_ORIGINAL);
	await expect(echoBChoice).toBeChecked();
	await expect(betaBChoice).toBeChecked();

	// A find that hides a selected track hides the row, never the
	// selection: count and hidden-count stay truthful while grouped.
	await field.fill('harbor');
	await expectSections(page, [['echo', ['grp-echo-a', 'grp-echo-b']]]);
	await expect(echoBChoice).toBeChecked();
	await expect(betaBChoice).toHaveCount(0);
	await expect(page.getByTestId('selection-count')).toHaveText('2 selected');
	await expect(page.getByTestId('selection-hidden-count')).toContainText(/1 selected.*hidden/i);
	await field.fill('');
	await expectSections(page, GROUPED_ORIGINAL);

	// Append selected queues exactly the selected tracks, in checkbox
	// activation order, regardless of their grouped positions.
	await page.getByRole('button', { name: 'Append selected', exact: true }).click();
	await expectQueue(page, ['grp-echo-b', 'grp-beta-b'], 'grp-echo-b', 'Harbor Song');
});

test('[GA-B06] row actions: exact accessible names act on the exact track despite the Harbor substring pair', async ({
	page
}) => {
	await mockLibrary(page);
	await openLibrary(page);
	const kind = await requireGroup(page);
	await setGrouped(page, kind, true);
	await expectSections(page, GROUPED_ORIGINAL);

	// Favorite: "Harbor Song" only — the substring title "Harbor" on the
	// sibling row in the same section must not be starred.
	await page.getByRole('button', { name: 'Star Harbor Song', exact: true }).click();
	await expect.poll(() => favoriteShelfIds(page)).toEqual(['grp-echo-b']);
	await expect(
		page.getByRole('button', { name: 'Star Harbor', exact: true }),
		'the substring sibling must stay unstarred'
	).toHaveAttribute('aria-pressed', 'false');
	// The pressed-state assertion addresses the browse row directly: once
	// the track is starred, the favorites shelf legitimately carries its
	// own "Unstar Harbor Song" control, so that exact name is no longer
	// page-unique. The Star lookups above stay page-wide — no shelf
	// control carries those names — which keeps the substring trap live.
	await expect(
		row(page, 'grp-echo-b').getByRole('button', { name: 'Unstar Harbor Song', exact: true })
	).toHaveAttribute('aria-pressed', 'true');

	// Select: "Harbor" only.
	await page.getByRole('checkbox', { name: 'Select Harbor', exact: true }).check();
	await expect(page.getByTestId('selection-count')).toHaveText('1 selected');
	await expect(
		page.getByRole('checkbox', { name: 'Select Harbor Song', exact: true })
	).not.toBeChecked();

	// Play/queue via the main row buttons, addressed by their exact flat
	// names (title — artist as shipped). "Harbor" is not on the shelf, so
	// its lookup stays page-wide; the shelf's favorite row duplicates
	// "Harbor Song — echo" exactly, so that click addresses the browse
	// row, like the Unstar assertion above.
	await page.getByRole('button', { name: 'Harbor — Echo', exact: true }).click();
	await expectQueue(page, ['grp-echo-a'], 'grp-echo-a', 'Harbor');
	await row(page, 'grp-echo-b')
		.getByRole('button', { name: 'Harbor Song — echo', exact: true })
		.click();
	await expectQueue(page, ['grp-echo-a', 'grp-echo-b'], 'grp-echo-a', 'Harbor');
	expect(await favoriteShelfIds(page), 'queueing must not change favorites').toEqual([
		'grp-echo-b'
	]);
	await expect(page.getByTestId('selection-count')).toHaveText('1 selected');
});

test('[GA-B07] view-only: toggling touches no network, media, queue, favorites, selection or storage, and off restores the flat list exactly', async ({
	page
}) => {
	const mutations: string[] = [];
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(error.message));
	page.on('request', (request) => {
		if (request.method() !== 'GET') mutations.push(`${request.method()} ${request.url()}`);
	});
	await page.addInitScript(() => {
		const calls = { play: 0, pause: 0 };
		Object.assign(window, { __groupMediaCalls: calls });
		for (const method of ['play', 'pause'] as const) {
			const original = HTMLMediaElement.prototype[method];
			Object.defineProperty(HTMLMediaElement.prototype, method, {
				configurable: true,
				value: function (this: HTMLMediaElement) {
					calls[method] += 1;
					return original.call(this);
				}
			});
		}
	});
	const api = await mockLibrary(page);
	await openLibrary(page);
	const kind = await requireGroup(page);

	// Rich pre-state under a non-original ordering.
	await row(page, 'grp-alpha').click();
	await row(page, 'grp-zulu-a').click();
	await page.getByTestId('player-next').click();
	await starButton(page, 'grp-beta-b').click();
	await page.getByRole('checkbox', { name: 'Select Beta Evening', exact: true }).check();
	await ordering(page).selectOption({ label: 'Title A–Z' });
	const flatOrder = [
		'grp-zulu-b',
		'grp-accent',
		'grp-alpha',
		'grp-beta-b',
		'grp-beta-a',
		'grp-echo-a',
		'grp-echo-b',
		'grp-orphan',
		'grp-blank',
		'grp-zulu-a'
	];
	await expectFlat(page, flatOrder);
	await expectQueue(page, ['grp-alpha', 'grp-zulu-a'], 'grp-zulu-a', 'Zebra Song');
	const queueBefore = await queueState(page);
	const storageBefore = await readStorage(page);
	const shelfBefore = await favoriteShelfIds(page);

	await setGrouped(page, kind, true);
	await expectSections(page, [
		['alpha', ['grp-alpha']],
		['beta band', ['grp-beta-b', 'grp-beta-a']],
		['echo', ['grp-echo-a', 'grp-echo-b']],
		['zulu band', ['grp-zulu-b', 'grp-zulu-a']],
		['éclair ensemble', ['grp-accent']],
		['unknown artist', ['grp-orphan', 'grp-blank']]
	]);
	expect(await queueState(page), 'queue/current/media untouched by grouping on').toEqual(
		queueBefore
	);
	expect(await readStorage(page), 'grouping writes no storage').toEqual(storageBefore);
	expect(await favoriteShelfIds(page)).toEqual(shelfBefore);
	await expect(
		page.getByRole('checkbox', { name: 'Select Beta Evening', exact: true })
	).toBeChecked();

	await setGrouped(page, kind, false);
	// Off restores today's flat list exactly: same rows, same order.
	await expectFlat(page, flatOrder);
	expect(await queueState(page), 'queue/current/media untouched by grouping off').toEqual(
		queueBefore
	);
	expect(await readStorage(page)).toEqual(storageBefore);
	expect(await favoriteShelfIds(page)).toEqual(shelfBefore);
	await expect(page.getByTestId('selection-count')).toHaveText('1 selected');
	await expect(ordering(page).locator('option:checked')).toHaveText('Title A–Z');
	expect(api.requests(), 'toggling must not fetch').toBe(1);
	expect(mutations, 'toggling must not mutate over the network').toEqual([]);
	expect(errors, 'toggling must not raise page errors').toEqual([]);
});

test('[GA-B08] structure and keyboard: real headings over lists, unchanged per-row tab order, polite feedback, 24px targets', async ({
	page
}) => {
	await mockLibrary(page);
	await openLibrary(page);
	const kind = await requireGroup(page);
	const toggle = groupToggle(page, kind);

	// The toggle is a native control reachable by Tab among the list
	// controls (above the list), with visible focus, operable by Space.
	const tag = await toggle.evaluate((element) => element.tagName);
	expect(tag, 'toggle must be a native form control').toMatch(/^(INPUT|BUTTON)$/);
	for (
		let tabs = 0;
		tabs < 40 && !(await toggle.evaluate((element) => element === document.activeElement));
		tabs += 1
	) {
		await page.keyboard.press('Tab');
	}
	await expect(toggle, '"Group by artist" must be reachable by Tab before the list').toBeFocused();
	const focusRing = await toggle.evaluate((element) => {
		const style = getComputedStyle(element);
		return {
			outline: style.outlineStyle !== 'none' && style.outlineWidth !== '0px',
			shadow: style.boxShadow !== 'none'
		};
	});
	expect(focusRing.outline || focusRing.shadow, 'toggle keyboard focus must be visible').toBe(
		true
	);
	await page.keyboard.press('Space');
	await expectGrouped(page, kind, true);
	await expectSections(page, GROUPED_ORIGINAL);
	const activeIsBody = await page.evaluate(() => document.activeElement === document.body);
	expect(activeIsBody, 'operating the toggle must not dump focus to <body>').toBe(false);

	// Headings are real heading elements below the h1, carry no tab stop,
	// and every row stays a list item inside a list.
	const structure = await page.evaluate((known) => {
		const headings = [...document.querySelectorAll('h1, h2, h3, h4, h5, h6')].filter(
			(node) =>
				(known as string[]).includes(
					(node.textContent ?? '').trim().replace(/\s+/g, ' ').toLowerCase()
				)
		);
		const rows = [...document.querySelectorAll('[data-testid="library-item"]')];
		return {
			headingTags: headings.map((node) => node.tagName),
			headingTabIndexes: headings.map((node) => node.getAttribute('tabindex')),
			rowsInLists: rows.every(
				(node) =>
					node.closest('ul, ol, [role="list"]') !== null &&
					(node.tagName === 'LI' || node.getAttribute('role') === 'listitem')
			)
		};
	}, [...SECTION_KEYS]);
	expect(structure.headingTags, 'one real heading per section').toHaveLength(SECTION_KEYS.length);
	for (const headingTag of structure.headingTags) {
		expect(headingTag, 'section headings sit below the page h1').toMatch(/^H[2-6]$/);
	}
	for (const tabIndex of structure.headingTabIndexes) {
		expect(
			tabIndex === null || Number(tabIndex) < 0,
			'section headings must not be tab stops'
		).toBe(true);
	}
	expect(structure.rowsInLists, 'grouped rows must stay list items inside lists').toBe(true);

	// Per-row keyboard order is the flat order, and a section heading
	// between the Alpha and Beta rows adds no tab stop.
	const alphaSelect = page.getByRole('checkbox', { name: 'Select Alpha Song', exact: true });
	await alphaSelect.focus();
	await expect(alphaSelect).toBeFocused();
	await page.keyboard.press('Tab');
	await expect(
		page.getByRole('button', { name: 'Alpha Song — Alpha', exact: true })
	).toBeFocused();
	await page.keyboard.press('Tab');
	await expect(
		page.getByRole('button', { name: 'Copy link Alpha Song', exact: true })
	).toBeFocused();
	await page.keyboard.press('Tab');
	await expect(page.getByRole('button', { name: 'Star Alpha Song', exact: true })).toBeFocused();
	await page.keyboard.press('Tab');
	await expect(
		page.getByRole('checkbox', { name: 'Select Beta Morning', exact: true }),
		'tab order must step straight from the last Alpha control to the first Beta row control'
	).toBeFocused();

	// Polite, main-scoped feedback: no assertive region in main and no
	// alert after toggling. (The SvelteKit announcer outside <main> is the
	// shipped baseline and excluded, as in the find banks.)
	await expect(
		page.locator('main [aria-live="assertive"]'),
		'grouping must not add assertive live regions in main'
	).toHaveCount(0);
	await expect(page.locator('[aria-live="assertive"]:not(#svelte-announcer)')).toHaveCount(0);
	await expect(page.getByRole('alert'), 'toggling must not raise alerts').toHaveCount(0);

	// Every interactive target in the grouped list, and the toggle itself,
	// is at least 24x24 CSS px.
	const toggleBox = await toggle.boundingBox();
	expect(toggleBox, 'toggle must have a target box').not.toBeNull();
	expect((toggleBox as { width: number }).width).toBeGreaterThanOrEqual(24);
	expect((toggleBox as { height: number }).height).toBeGreaterThanOrEqual(24);
	const smallTargets = await page.evaluate(() => {
		const small: string[] = [];
		for (const item of document.querySelectorAll('[data-testid="library-item"]')) {
			for (const control of item.querySelectorAll('input, button')) {
				const rect = control.getBoundingClientRect();
				if (rect.width < 24 || rect.height < 24) {
					small.push(
						`${item.getAttribute('data-id')}: ${control.tagName} ${
							control.getAttribute('aria-label') ?? control.textContent?.trim() ?? ''
						} ${rect.width}x${rect.height}`
					);
				}
			}
		}
		return small;
	});
	expect(smallTargets, 'grouped row controls must stay >= 24x24 CSS px').toEqual([]);
});

test.describe('touch and narrow viewport', () => {
	test.use({ hasTouch: true });

	test('[GA-B09] 320px light/dark: grouped sections stay visible and touch-operable with no horizontal scroll', async ({
		page
	}) => {
		await mockLibrary(page);
		await page.setViewportSize({ width: 320, height: 700 });
		await openLibrary(page);
		const kind = await requireGroup(page);
		await groupToggle(page, kind).tap();
		await expectGrouped(page, kind, true);
		await expectSections(page, GROUPED_ORIGINAL);

		for (const scheme of ['dark', 'light'] as const) {
			await page.emulateMedia({ colorScheme: scheme });
			await expect(groupToggle(page, kind), `toggle visible in ${scheme} mode`).toBeVisible();
			await expect(
				page.getByRole('heading', { name: /^alpha$/i }),
				`first artist heading visible in ${scheme} mode`
			).toBeVisible();
			await expect(row(page, 'grp-alpha'), `grouped row visible in ${scheme} mode`).toBeVisible();
			const overflow = await page.evaluate(
				() => document.documentElement.scrollWidth - document.documentElement.clientWidth
			);
			expect(overflow, `no horizontal scrolling at 320px in ${scheme} mode`).toBeLessThanOrEqual(
				1
			);
		}

		await groupToggle(page, kind).tap();
		await expectGrouped(page, kind, false);
		await expectFlat(page, API_IDS);
	});
});

test('[GA-B10] reload: grouping is visit-local, resets to the flat list, and wrote no storage', async ({
	page
}) => {
	await mockLibrary(page);
	await openLibrary(page);
	const kind = await requireGroup(page);
	const storageBefore = await readStorage(page);
	await setGrouped(page, kind, true);
	await expectSections(page, GROUPED_ORIGINAL);
	expect(await readStorage(page), 'enabling grouping must persist nothing').toEqual(
		storageBefore
	);

	await page.reload();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	const kindAfter = await requireGroup(page);
	expect(kindAfter).toBe(kind);
	await expectGrouped(page, kindAfter, false);
	await expectFlat(page, API_IDS);
});

test('[GA-B11] account teardown: the next account starts ungrouped with its own flat list', async ({
	page
}) => {
	const reply: LibraryReply = { items: ITEMS };
	await mockLibrary(page, reply);
	await openLibrary(page);
	const kind = await requireGroup(page);
	await setGrouped(page, kind, true);
	await expectSections(page, GROUPED_ORIGINAL);

	await authTransition(page);
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('library-item')).toHaveCount(0);

	reply.items = NEXT_ACCOUNT;
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	// API order is Zeta then Alpha: leaked grouping would reorder the rows
	// and render Alpha Next / Zeta Next headings for the new account.
	await expectRows(page, ['next-zeta', 'next-alpha']);
	const kindAfter = await requireGroup(page);
	expect(kindAfter).toBe(kind);
	await expectGrouped(page, kindAfter, false);
	await expectFlat(page, ['next-zeta', 'next-alpha'], NEXT_KEYS);
});
