import { expect, test, type Locator, type Page, type TestInfo } from '@playwright/test';

/**
 * Acceptance-owned: the paired build makes this file pass without editing
 * it. Offline browser contract for finding canonically equivalent track
 * names across the three find surfaces — the loaded-library find, the
 * library PlayerQueue filter and the demo queue filter — not
 * implementation/source-shape pins. The pure halves are
 * `src/lib/library/canonical-find-acceptance.test.ts` and
 * `src/lib/player/canonical-queue-find-acceptance.test.ts`. The existing
 * library-local-filter, library-find-scope, library-queue-filter,
 * library-queue-filter-pins, demo-queue-filter and ordering/favorites
 * banks stay protected and unedited, and every behavior they pin must
 * keep passing.
 *
 * Matching contract (mirrors the pure halves): NFC canonical equivalence
 * over title/artist metadata in both query directions; accents stay
 * significant; compatibility-only lookalikes never match; the stable-id
 * fallback stays byte-literal (case-fold/trim only); metacharacters stay
 * literal; stored metadata, ids, the typed query text and row order are
 * never rewritten. Filtering is presentation-only: truthful counts, no
 * requests, no queue/favorites/selection/storage/media change.
 *
 * Fixture discipline: visible title/artist text is digit-free and avoids
 * the words "star" and "favorite". Rows canon-eclair/canon-decomposed,
 * canon-ring/canon-ring-back, canon-combined/canon-combined-back and
 * canon-hangul/canon-hangul-back store the composed and decomposed
 * spelling of one name. Every RED case first runs an ASCII control query
 * proving the surface filters at all, so RED always means the canonical
 * behavior is missing — never a load, hydration or selector failure.
 *
 * The demo queue ships fixed ASCII tracks, so NFC is a no-op there by
 * construction: the demo half pins controls (baseline match, ligature and
 * combining-mark non-matches, literal metacharacters, no-mutation) that
 * pass before AND after the build and prove the shared-matcher change
 * did not alter demo behavior.
 *
 * Case IDs (stable): CF-B00 fixture guard, CF-B01 composed/decomposed
 * title both directions, CF-B02 keyboard flow and text preservation,
 * CF-B03 artist both directions with scope narrowing, CF-B04 combined
 * marks and Hangul, CF-B05 accent/compatibility negatives, CF-B06 id
 * fallback byte-literal, CF-B07 presentation-only find, CF-B08 library
 * queue canonical filter, CF-B09 library queue id/literal controls,
 * CF-B10 demo unicode controls, CF-B11 demo no-mutation.
 *
 * Focused command (repo root, deterministic, offline):
 *   CI=1 PUBLIC_API_BASE_URL='' pnpm --dir apps/web exec playwright test tests/canonical-track-find.spec.ts --workers=1 --retries=0
 */

// Canonical pairs as literal UTF-8 (see the pure halves for the scalar
// identities); [CF-B00] pins every code point numerically. Outside these
// constants the file is pure ASCII.
const E_ACUTE = 'é';
const E_DECOMPOSED = 'é';
const A_RING = 'Å';
const A_RING_DECOMPOSED = 'Å';
const A_CIRCUMFLEX_ACUTE = 'ấ';
const A_CIRCUMFLEX_ACUTE_DECOMPOSED = 'ấ';
const HANGUL_GA = '가';
const HANGUL_GA_DECOMPOSED = '가';
const LIGATURE_FI = 'ﬁ';

const ITEMS = [
	{ id: 'canon-eclair', title: `${E_ACUTE}clair`, artist: `${E_ACUTE}cho Band` },
	{ id: 'canon-decomposed', title: `${E_DECOMPOSED}clair Tune`, artist: 'Plain Band' },
	{ id: 'canon-ring', title: 'Northern Song', artist: `${A_RING}ngstrom` },
	{ id: 'canon-ring-back', title: 'Southern Song', artist: `${A_RING_DECOMPOSED}ngstrom` },
	{ id: 'canon-combined', title: `S${A_CIRCUMFLEX_ACUTE}o Paulo Nights`, artist: 'Coast Band' },
	{ id: 'canon-combined-back', title: `S${A_CIRCUMFLEX_ACUTE_DECOMPOSED}o Paulo Days`, artist: 'Harbor Band' },
	{ id: 'canon-hangul', title: `${HANGUL_GA} Morning`, artist: 'Seoul Band' },
	{ id: 'canon-hangul-back', title: `${HANGUL_GA_DECOMPOSED} Evening`, artist: 'River Band' },
	{ id: 'canon-bare', title: 'Eclair', artist: 'Echo' },
	{ id: 'canon-ligature', title: `${LIGATURE_FI}nishing Song`, artist: 'Close Band' },
	{ id: `canon-${E_DECOMPOSED}-id` },
	{ id: 'canon-plain', title: 'Plain Tune', artist: 'Plain Band' }
];
const API_IDS = ITEMS.map((item) => item.id);
const DECOMPOSED_ID = `canon-${E_DECOMPOSED}-id`;
const COMPOSED_ID_FORM = `canon-${E_ACUTE}-id`;

test.setTimeout(30_000);
test.use({ hasTouch: true });

const points = (value: string): (number | undefined)[] =>
	[...value].map((character) => character.codePointAt(0));

async function receipt(info: TestInfo, expected: unknown, observe: () => Promise<unknown>) {
	let actual: unknown = { error: 'case did not run' };
	let passed = false;
	try {
		actual = await observe();
		expect(actual).toEqual(expected);
		passed = true;
	} catch (error) {
		if (actual !== null && typeof actual === 'object' && 'error' in (actual as object)) {
			actual = { error: error instanceof Error ? error.message : String(error) };
		}
		throw error;
	} finally {
		const line = JSON.stringify({
			case_id: info.title.split(' ')[0],
			expected,
			actual,
			status: passed ? 'PASS' : 'FAIL',
			error_code: passed ? null : 'CANONICAL_TRACK_FIND_CONTRACT',
			next_action: passed
				? 'none'
				: 'Apply NFC canonical equivalence to title/artist matching on every find surface; keep ids byte-literal.'
		});
		console.log(line);
		await info.attach('acceptance-case', { body: line, contentType: 'application/json' });
	}
}

/* ---- library surface helpers ---- */

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

function findField(page: Page): Locator {
	return page
		.getByRole('searchbox', { name: 'Find in loaded tracks' })
		.or(page.getByRole('textbox', { name: 'Find in loaded tracks' }));
}

function scopeControl(page: Page): Locator {
	return page.getByLabel('Search in', { exact: true });
}

function clearSearch(page: Page): Locator {
	return page.getByRole('button', { name: /clear search/i }).first();
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

function findCountText(visible: number, loaded: number): string {
	return `Showing ${visible} of ${loaded} ${visible === 1 ? 'track' : 'tracks'}`;
}

async function expectFindCount(page: Page, visible: number, loaded = ITEMS.length) {
	await expect(page.getByTestId('library-find-count')).toHaveText(findCountText(visible, loaded));
}

async function expectNoLibraryMatches(page: Page) {
	await expect(page.getByTestId('library-item')).toHaveCount(0);
	await expect(page.locator('main').getByText('No matches in your loaded tracks.')).toBeVisible();
	await expect(clearSearch(page)).toBeVisible();
}

async function instrument(page: Page) {
	await page.addInitScript(() => {
		const probe = { media: [] as string[], storage: [] as string[] };
		(window as unknown as Record<string, unknown>)['canonProbe'] = probe;
		for (const method of ['play', 'pause', 'load'] as const) {
			Object.defineProperty(HTMLMediaElement.prototype, method, {
				configurable: true,
				value() {
					probe.media.push(method);
					return method === 'play' ? Promise.resolve() : undefined;
				}
			});
		}
		for (const method of ['setItem', 'removeItem', 'clear'] as const) {
			const native = Storage.prototype[method];
			Object.defineProperty(Storage.prototype, method, {
				configurable: true,
				value(...args: unknown[]) {
					probe.storage.push(method);
					return Reflect.apply(native, this, args);
				}
			});
		}
	});
}

async function probe(page: Page): Promise<{ media: string[]; storage: string[] }> {
	return page.evaluate(() => (window as unknown as Record<string, { media: string[]; storage: string[] }>)['canonProbe']);
}

async function resetProbe(page: Page) {
	await page.evaluate(() => {
		const current = (window as unknown as Record<string, { media: string[]; storage: string[] }>)['canonProbe'];
		current.media.length = 0;
		current.storage.length = 0;
	});
}

/* ---- library queue surface helpers ---- */

const queueInput = (page: Page) =>
	page
		.getByRole('textbox', { name: 'Filter queue', exact: true })
		.or(page.getByRole('searchbox', { name: 'Filter queue', exact: true }));
const queueClear = (page: Page) => page.getByRole('button', { name: 'Clear queue filter', exact: true });
const queueOrder = (page: Page) =>
	page.getByTestId('queue-item').evaluateAll((rows) => rows.map((row) => row.getAttribute('data-track-id')));
const queueRow = (page: Page, id: string) => page.locator(`[data-testid="queue-item"][data-track-id="${id}"]`);

async function seedQueue(page: Page) {
	for (const item of ITEMS) {
		await page.locator(`[data-testid="library-item"][data-id="${item.id}"] button.nt-library-row`).click();
	}
	await expect(page.getByTestId('queue-item')).toHaveCount(ITEMS.length);
}

const settle = (page: Page) =>
	page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));

/* ---- demo surface helpers ---- */

type DemoProbe = { play: number; pause: number; load: number; seek: number; audio: HTMLAudioElement | null };
declare global {
	interface Window {
		__canonDemo?: DemoProbe;
	}
}
const DEMO_ORDER = ['demo-track', 'second-signal', 'third-light'];

async function openDemo(page: Page, baseURL: string) {
	const origin = new URL(baseURL);
	expect(['localhost', '127.0.0.1', '[::1]']).toContain(origin.hostname);
	await page.route('**/*', (route) => {
		const target = new URL(route.request().url());
		if (target.origin !== origin.origin) return route.abort();
		// Keep browser media pending locally; deterministic media doubles below
		// isolate the external decoder, not the real playback state machine.
		if (target.pathname === '/api/demo/audio') return;
		if (target.pathname.startsWith('/api/')) return route.abort();
		return route.continue();
	});
	await page.addInitScript(() => {
		const probe: DemoProbe = { play: 0, pause: 0, load: 0, seek: 0, audio: null };
		window.__canonDemo = probe;
		const times = new WeakMap<HTMLMediaElement, number>();
		Object.defineProperty(HTMLMediaElement.prototype, 'currentTime', {
			configurable: true,
			get() {
				return times.get(this) ?? 0;
			},
			set(value: number) {
				probe.seek += 1;
				times.set(this, value);
			}
		});
		HTMLMediaElement.prototype.play = function () {
			probe.play += 1;
			Object.defineProperty(this, 'paused', { value: false, configurable: true });
			Object.defineProperty(this, 'ended', { value: false, configurable: true });
			return Promise.resolve();
		};
		HTMLMediaElement.prototype.pause = function () {
			probe.pause += 1;
			Object.defineProperty(this, 'paused', { value: true, configurable: true });
		};
		HTMLMediaElement.prototype.load = function () {
			probe.load += 1;
		};
	});
	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	// SSR rows alone are insufficient: this real mutation proves hydration.
	await page.getByTestId('queue-move-up-third-light').click();
	await expect.poll(() => queueOrder(page)).toEqual(['demo-track', 'third-light', 'second-signal']);
	await page.getByTestId('queue-move-down-third-light').click();
	await expect.poll(() => queueOrder(page)).toEqual(DEMO_ORDER);
	await settle(page);
}

async function demoLive(page: Page) {
	return (await page.getByRole('status').allTextContents()).join(' ').replace(/\s+/g, ' ');
}

async function demoSnapshot(page: Page) {
	return page.getByTestId('demo-audio').evaluate((audio: HTMLAudioElement) => {
		const probe = window.__canonDemo!;
		const metadata = navigator.mediaSession?.metadata;
		return {
			src: audio.getAttribute('src'),
			time: audio.currentTime,
			paused: audio.paused,
			rate: audio.playbackRate,
			calls: { play: probe.play, pause: probe.pause, load: probe.load, seek: probe.seek },
			metadata: metadata ? { title: metadata.title, artist: metadata.artist } : null,
			local: { ...localStorage },
			session: { ...sessionStorage },
			now: document.querySelector('[data-testid="now-playing"]')?.textContent,
			status: document.querySelector('[data-testid="player-status"]')?.textContent
		};
	});
}

/* ---- cases ---- */

test('CF-B00 fixture constants hold their intended code points', async ({}, info) => {
	await receipt(info, 'fixture byte shapes intact', async () => {
		expect(points(E_ACUTE)).toEqual([0xe9]);
		expect(points(E_DECOMPOSED)).toEqual([0x65, 0x301]);
		expect(points(A_RING)).toEqual([0xc5]);
		expect(points(A_RING_DECOMPOSED)).toEqual([0x41, 0x30a]);
		expect(points(A_CIRCUMFLEX_ACUTE)).toEqual([0x1ea5]);
		expect(points(A_CIRCUMFLEX_ACUTE_DECOMPOSED)).toEqual([0x61, 0x302, 0x301]);
		expect(points(HANGUL_GA)).toEqual([0xac00]);
		expect(points(HANGUL_GA_DECOMPOSED)).toEqual([0x1100, 0x1161]);
		expect(points(LIGATURE_FI)).toEqual([0xfb01]);
		expect(E_DECOMPOSED.normalize('NFC')).toBe(E_ACUTE);
		expect(A_RING_DECOMPOSED.normalize('NFC')).toBe(A_RING);
		expect(A_CIRCUMFLEX_ACUTE_DECOMPOSED.normalize('NFC')).toBe(A_CIRCUMFLEX_ACUTE);
		expect(HANGUL_GA_DECOMPOSED.normalize('NFC')).toBe(HANGUL_GA);
		return 'fixture byte shapes intact';
	});
});

test('CF-B01 decomposed and composed title queries match both stored spellings', async ({ page }, info) => {
	await mockLibrary(page);
	await openLibrary(page);
	await receipt(info, { control: ['canon-bare'], decomposed: ['canon-eclair', 'canon-decomposed'], composed: ['canon-eclair', 'canon-decomposed'] }, async () => {
		const field = findField(page);
		await expect(field, 'library must expose the labeled find-in-loaded-tracks field').toBeVisible();
		await field.fill('eclair');
		await expectRows(page, ['canon-bare']);
		const control = await page.getByTestId('library-item').evaluateAll((rows) => rows.map((row) => row.getAttribute('data-id')));
		await field.fill(`${E_DECOMPOSED}clair`);
		await expectRows(page, ['canon-eclair', 'canon-decomposed']);
		await expectFindCount(page, 2);
		const decomposed = await page.getByTestId('library-item').evaluateAll((rows) => rows.map((row) => row.getAttribute('data-id')));
		await field.fill(`${E_ACUTE}clair`);
		await expectRows(page, ['canon-eclair', 'canon-decomposed']);
		await expectFindCount(page, 2);
		const composed = await page.getByTestId('library-item').evaluateAll((rows) => rows.map((row) => row.getAttribute('data-id')));
		return { control, decomposed, composed };
	});
});

test('CF-B02 keyboard entry preserves the typed spelling and rendered metadata', async ({ page }, info) => {
	await mockLibrary(page);
	await openLibrary(page);
	await receipt(info, { rows: ['canon-eclair', 'canon-decomposed'], valuePoints: points(`${E_DECOMPOSED}clair`), renderedPoints: points(`${E_ACUTE}clair`) }, async () => {
		const field = findField(page);
		await field.tap();
		await field.pressSequentially(`${E_DECOMPOSED}clair`, { delay: 5 });
		await expectRows(page, ['canon-eclair', 'canon-decomposed']);
		await expectFindCount(page, 2);
		// The input keeps the exact typed spelling: the find never rewrites
		// the query to its normalized form.
		const value = await field.inputValue();
		// Rendered metadata keeps its stored spelling: the composed row
		// still renders the composed title.
		const rendered = await page
			.locator('[data-testid="library-item"][data-id="canon-eclair"]')
			.textContent();
		expect(rendered).toContain(`${E_ACUTE}clair`);
		const decomposedRendered = await page
			.locator('[data-testid="library-item"][data-id="canon-decomposed"]')
			.textContent();
		expect(decomposedRendered).toContain(`${E_DECOMPOSED}clair`);
		const rows = await page.getByTestId('library-item').evaluateAll((rows) => rows.map((row) => row.getAttribute('data-id')));
		return { rows, valuePoints: points(value), renderedPoints: points(`${E_ACUTE}clair`) };
	});
});

test('CF-B03 artist equivalence composes with the title/artist scopes', async ({ page }, info) => {
	await mockLibrary(page);
	await openLibrary(page);
	await receipt(info, { both: ['canon-ring', 'canon-ring-back'], title: [], artist: ['canon-ring', 'canon-ring-back'] }, async () => {
		const field = findField(page);
		const scope = scopeControl(page);
		await expect(scope, 'library find must expose the shipped Search in selector').toBeVisible();
		await field.fill(`${A_RING_DECOMPOSED}ngstrom`);
		await expectRows(page, ['canon-ring', 'canon-ring-back']);
		await expectFindCount(page, 2);
		const both = await page.getByTestId('library-item').evaluateAll((rows) => rows.map((row) => row.getAttribute('data-id')));
		await scope.selectOption({ label: 'Title only' });
		await expect(page.getByTestId('library-item')).toHaveCount(0);
		const title: (string | null)[] = await page.getByTestId('library-item').evaluateAll((rows) => rows.map((row) => row.getAttribute('data-id')));
		await scope.selectOption({ label: 'Artist only' });
		await expectRows(page, ['canon-ring', 'canon-ring-back']);
		const artist = await page.getByTestId('library-item').evaluateAll((rows) => rows.map((row) => row.getAttribute('data-id')));
		// The query text survives scope changes.
		await expect(field).toHaveValue(`${A_RING_DECOMPOSED}ngstrom`);
		return { both, title, artist };
	});
});

test('CF-B04 combined marks and Hangul match in both directions', async ({ page }, info) => {
	await mockLibrary(page);
	await openLibrary(page);
	await receipt(info, { combined: ['canon-combined', 'canon-combined-back'], hangul: ['canon-hangul', 'canon-hangul-back'] }, async () => {
		const field = findField(page);
		await field.fill(`s${A_CIRCUMFLEX_ACUTE}o`);
		await expectRows(page, ['canon-combined', 'canon-combined-back']);
		await expectFindCount(page, 2);
		const combined = await page.getByTestId('library-item').evaluateAll((rows) => rows.map((row) => row.getAttribute('data-id')));
		await field.fill(` ${HANGUL_GA_DECOMPOSED} `);
		await expectRows(page, ['canon-hangul', 'canon-hangul-back']);
		await expectFindCount(page, 2);
		const hangul = await page.getByTestId('library-item').evaluateAll((rows) => rows.map((row) => row.getAttribute('data-id')));
		return { combined, hangul };
	});
});

test('CF-B05 bare vowels and compatibility lookalikes never match accented rows', async ({ page }, info) => {
	await mockLibrary(page);
	await openLibrary(page);
	await receipt(info, { bare: ['canon-bare'], echo: ['canon-bare'], ligature: ['canon-ligature'], finishing: [], dots: [] }, async () => {
		const field = findField(page);
		await field.fill('eclair');
		await expectRows(page, ['canon-bare']);
		const bare = await page.getByTestId('library-item').evaluateAll((rows) => rows.map((row) => row.getAttribute('data-id')));
		await field.fill('echo');
		await expectRows(page, ['canon-bare']);
		const echo = await page.getByTestId('library-item').evaluateAll((rows) => rows.map((row) => row.getAttribute('data-id')));
		await field.fill(`${LIGATURE_FI}nishing`);
		await expectRows(page, ['canon-ligature']);
		const ligature = await page.getByTestId('library-item').evaluateAll((rows) => rows.map((row) => row.getAttribute('data-id')));
		// No NFKC: the ASCII lookalike matches nothing at all.
		await field.fill('finishing');
		await expectNoLibraryMatches(page);
		const finishing: (string | null)[] = await page.getByTestId('library-item').evaluateAll((rows) => rows.map((row) => row.getAttribute('data-id')));
		// Metacharacters stay literal: no row contains the text '.*'.
		await field.fill('.*');
		await expectNoLibraryMatches(page);
		const dots: (string | null)[] = await page.getByTestId('library-item').evaluateAll((rows) => rows.map((row) => row.getAttribute('data-id')));
		return { bare, echo, ligature, finishing, dots };
	});
});

test('CF-B06 the untitled id fallback stays byte-literal in both directions', async ({ page }, info) => {
	await mockLibrary(page);
	await openLibrary(page);
	await receipt(info, { exact: [DECOMPOSED_ID], composedForm: [], titled: [] }, async () => {
		const field = findField(page);
		await field.fill(DECOMPOSED_ID);
		await expectRows(page, [DECOMPOSED_ID]);
		await expectFindCount(page, 1);
		const exact = await page.getByTestId('library-item').evaluateAll((rows) => rows.map((row) => row.getAttribute('data-id')));
		// Canonically equivalent but byte-different: the id must NOT match.
		await field.fill(COMPOSED_ID_FORM);
		await expectNoLibraryMatches(page);
		const composedForm: (string | null)[] = await page.getByTestId('library-item').evaluateAll((rows) => rows.map((row) => row.getAttribute('data-id')));
		// A titled row is never matched by its own id.
		await field.fill('canon-eclair');
		await expectNoLibraryMatches(page);
		const titled: (string | null)[] = await page.getByTestId('library-item').evaluateAll((rows) => rows.map((row) => row.getAttribute('data-id')));
		return { exact, composedForm, titled };
	});
});

test('CF-B07 finding makes no request and touches no queue, selection, favorites, storage or media', async ({ page }, info) => {
	await instrument(page);
	const library = await mockLibrary(page);
	await openLibrary(page);
	await receipt(info, { restored: API_IDS, hidden: '1 selected track is hidden by this find.', libraryCalls: 1, requests: [], media: [], storageWrites: [], unstarred: 0 }, async () => {
		const callsBefore = library.requests();
		const stored = await page.evaluate(() => ({ local: { ...localStorage }, session: { ...sessionStorage } }));
		const requests: string[] = [];
		page.on('request', (request) => requests.push(`${request.method()} ${new URL(request.url()).pathname}`));
		await resetProbe(page);
		const field = findField(page);
		// Hidden-selection behavior: the checked row stays checked while hidden.
		await page.locator('[data-testid="library-item"][data-id="canon-plain"] input.nt-library-select').check();
		await field.fill('eclair');
		await expectRows(page, ['canon-bare']);
		const hidden = await page.getByTestId('selection-hidden-count').textContent();
		await field.fill(`${E_DECOMPOSED}clair`);
		await field.fill('finishing');
		await expect(clearSearch(page)).toBeVisible();
		await clearSearch(page).click();
		await expectRows(page, API_IDS);
		await expectFindCount(page, ITEMS.length);
		const restored = await page.getByTestId('library-item').evaluateAll((rows) => rows.map((row) => row.getAttribute('data-id')));
		const stillChecked = await page.locator('[data-testid="library-item"][data-id="canon-plain"] input.nt-library-select').isChecked();
		expect(stillChecked).toBe(true);
		expect(await page.evaluate(() => ({ local: { ...localStorage }, session: { ...sessionStorage } }))).toEqual(stored);
		const effects = await probe(page);
		await expect(page.locator('audio')).toHaveCount(0);
		await expect(page.getByTestId('queue-item')).toHaveCount(0);
		const unstarred = await page.getByRole('button', { name: /^Unstar / }).count();
		return { restored, hidden: hidden?.replace(/\s+/g, ' ').trim(), libraryCalls: library.requests() - callsBefore + 1, requests, media: effects.media, storageWrites: effects.storage, unstarred };
	});
});

test('CF-B08 library queue filter matches canonically equivalent queued names', async ({ page, baseURL }, info) => {
	const origin = new URL(baseURL!);
	expect(['localhost', '127.0.0.1', '[::1]']).toContain(origin.hostname);
	await instrument(page);
	const library = await mockLibrary(page);
	await openLibrary(page);
	await seedQueue(page);
	await page.getByTestId('player-next').click();
	await expect(page.locator('[data-testid="queue-item"][aria-current="true"]')).toHaveAttribute('data-track-id', 'canon-decomposed');
	await receipt(info, { control: ['canon-bare'], canonical: ['canon-eclair', 'canon-decomposed'], count: '2 of 12 tracks', currentHidden: true, restored: API_IDS, libraryCalls: 1, requests: [] }, async () => {
		const callsBefore = library.requests();
		const requests: string[] = [];
		page.on('request', (request) => requests.push(`${request.method()} ${new URL(request.url()).pathname}`));
		await resetProbe(page);
		const input = queueInput(page);
		await expect(input, 'library queue must expose the labeled Filter queue input').toBeVisible();
		await input.fill('eclair');
		await expect.poll(() => queueOrder(page)).toEqual(['canon-bare']);
		const control = await queueOrder(page);
		await input.fill(`${E_DECOMPOSED}clair`);
		await expect.poll(() => queueOrder(page)).toEqual(['canon-eclair', 'canon-decomposed']);
		await expect(page.getByTestId('queue-filter-count')).toHaveText('2 of 12 tracks');
		const count = await page.getByTestId('queue-filter-count').textContent();
		const canonical = await queueOrder(page);
		// The current track stays selected while hidden; the position still
		// describes the full queue, never the filtered rows.
		await input.fill(`${A_RING}ngstrom`);
		await expect.poll(() => queueOrder(page)).toEqual(['canon-ring', 'canon-ring-back']);
		const currentHidden = (await page.locator('[data-testid="queue-item"][aria-current="true"]').count()) === 0;
		await expect(page.getByTestId('queue-position')).toContainText('Track 2 of 12');
		await queueClear(page).click();
		await expect.poll(() => queueOrder(page)).toEqual(API_IDS);
		await expect(page.getByTestId('queue-filter-count')).toHaveText('12 of 12 tracks');
		await expect(page.locator('[data-testid="queue-item"][aria-current="true"]')).toHaveAttribute('data-track-id', 'canon-decomposed');
		const restored = await queueOrder(page);
		const effects = await probe(page);
		expect(effects.media).toEqual([]);
		await expect(page.locator('audio')).toHaveCount(0);
		return { control, canonical, count: count?.trim(), currentHidden, restored, libraryCalls: library.requests() - callsBefore + 1, requests };
	});
});

test('CF-B09 library queue keeps id, literal and no-match behavior byte-literal', async ({ page, baseURL }, info) => {
	expect(['localhost', '127.0.0.1', '[::1]']).toContain(new URL(baseURL!).hostname);
	await mockLibrary(page);
	await openLibrary(page);
	await seedQueue(page);
	await receipt(info, { exact: [DECOMPOSED_ID], composedForm: [], named: [], dots: [], empty: true, restored: API_IDS }, async () => {
		const input = queueInput(page);
		await input.fill(DECOMPOSED_ID);
		await expect.poll(() => queueOrder(page)).toEqual([DECOMPOSED_ID]);
		const exact = await queueOrder(page);
		await input.fill(COMPOSED_ID_FORM);
		await expect.poll(() => queueOrder(page)).toEqual([]);
		await expect(page.getByTestId('queue-filter-count')).toHaveText('0 of 12 tracks');
		await expect(page.getByTestId('queue-filter-empty')).toContainText(/no matches/i);
		const composedForm = await queueOrder(page);
		await input.fill('canon-eclair');
		await expect.poll(() => queueOrder(page)).toEqual([]);
		const named = await queueOrder(page);
		await input.fill('.*');
		await expect.poll(() => queueOrder(page)).toEqual([]);
		const dots = await queueOrder(page);
		const empty = (await page.getByTestId('queue-filter-empty').count()) === 1;
		await queueClear(page).click();
		await expect.poll(() => queueOrder(page)).toEqual(API_IDS);
		const restored = await queueOrder(page);
		return { exact, composedForm, named, dots, empty, restored };
	});
});

test('CF-B10 demo queue keeps ASCII matching while unicode lookalikes match nothing', async ({ page, baseURL }, info) => {
	await openDemo(page, baseURL!);
	await receipt(info, { control: ['second-signal'], ligature: [], decomposed: [], dots: [] }, async () => {
		const input = queueInput(page);
		if ((await input.count()) !== 1) return { error: 'demo queue-only Filter queue input missing' };
		await input.fill('  sIgNaL  ');
		await settle(page);
		await expect.poll(() => queueOrder(page)).toEqual(['second-signal']);
		expect(await demoLive(page)).toContain('1 of 3 tracks');
		const control = await queueOrder(page);
		await input.fill(`${LIGATURE_FI}xture`);
		await settle(page);
		await expect.poll(() => queueOrder(page)).toEqual([]);
		expect(await demoLive(page)).toContain('0 of 3 tracks');
		expect(await demoLive(page)).toContain('No matches in this queue');
		const ligature = await queueOrder(page);
		await input.fill(`${E_DECOMPOSED}mo`);
		await settle(page);
		await expect.poll(() => queueOrder(page)).toEqual([]);
		const decomposed = await queueOrder(page);
		await input.fill('.*');
		await settle(page);
		await expect.poll(() => queueOrder(page)).toEqual([]);
		const dots = await queueOrder(page);
		await queueClear(page).click();
		await settle(page);
		await expect.poll(() => queueOrder(page)).toEqual(DEMO_ORDER);
		return { control, ligature, decomposed, dots };
	});
});

test('CF-B11 demo unicode filtering leaves playback, order, selection and storage untouched', async ({ page, baseURL }, info) => {
	await openDemo(page, baseURL!);
	await page.getByTestId('queue-select-second-signal').click();
	await page.getByTestId('demo-audio').evaluate((audio: HTMLAudioElement) => {
		audio.dispatchEvent(new Event('playing'));
		audio.currentTime = 42.5;
		audio.dispatchEvent(new Event('timeupdate'));
		window.__canonDemo!.audio = audio;
	});
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await settle(page);
	const before = await demoSnapshot(page);
	expect(before.time).toBe(42.5);
	const requests: string[] = [];
	page.on('request', (request) => requests.push(`${request.method()} ${new URL(request.url()).pathname}`));
	await receipt(info, { filtered: [], restored: DEMO_ORDER, unchanged: true, requests: [] }, async () => {
		const input = queueInput(page);
		if ((await input.count()) !== 1) return { error: 'demo queue-only Filter queue input missing' };
		await input.tap();
		await input.pressSequentially(`${LIGATURE_FI}xture ${E_DECOMPOSED}mo`, { delay: 5 });
		await settle(page);
		const filtered = await queueOrder(page);
		const during = await demoSnapshot(page);
		await input.fill('');
		await input.pressSequentially('k j m r ');
		await input.press('ArrowLeft');
		await input.press('Enter');
		await queueClear(page).click();
		await settle(page);
		const after = await demoSnapshot(page);
		const current = await page.evaluate(() => JSON.parse(sessionStorage.getItem('nntune:demo-queue:v1') ?? 'null')?.currentId);
		expect(current).toBe('second-signal');
		return { filtered, restored: await queueOrder(page), unchanged: JSON.stringify(during) === JSON.stringify(before) && JSON.stringify(after) === JSON.stringify(before), requests };
	});
});
