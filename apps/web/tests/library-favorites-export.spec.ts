import { expect, test, type Download, type Locator, type Page } from '@playwright/test';
import { mkdir, readFile } from 'node:fs/promises';
import { dirname } from 'node:path';

/**
 * Test-Author owned RED acceptance for FAVEXPORT: export the device-local
 * favorites from the library page Favorites section, in arranged
 * (star/move) order, as a downloaded M3U or as copied text. The Builder
 * wires the library page to satisfy this file plus
 * `apps/web/src/lib/library/favoritesExport.acceptance.test.ts` and must
 * not edit either file. Real library page, scoped browser storage and
 * queue; only HTTP is mocked. No proposed-helper imports: RED must be a
 * browser assertion, not a missing API.
 *
 * LOCKED PAGE CONTRACT (Favorites section of `/library`):
 * - `favorites-export-copy`: NATIVE <button> with accessible name exactly
 *   "Copy favorites as text". `favorites-export-download`: NATIVE
 *   <button> with accessible name exactly "Download favorites (.m3u)".
 *   Both live in the Favorites section, are keyboard reachable and
 *   operable (Enter/Space), and are at least 24x24 CSS px. While no
 *   favorite is listed they are absent or disabled — never an enabled
 *   action that produces an empty "successful" export.
 * - Bytes come from the single shared queue formatting core
 *   (`formatQueueText` / `formatQueueM3U`) over exactly the favorites the
 *   section lists, in arranged order at activation time: numbered
 *   `N. <title>[ — <artist>]` lines for copy; `#EXTM3U` +
 *   `#EXTINF:-1,<label>` + bare stable id lines, downloaded as
 *   `favorites.m3u`. Stored ids absent from the loaded library are
 *   skipped in the bytes and stay stored. Never stream URLs, tokens,
 *   provider info, or identity.
 * - A listed favorite whose id the shared guard refuses makes BOTH
 *   actions refuse truthfully: an in-page error, no clipboard write, no
 *   download (not even a partial one), no false "Copied"/"Downloaded",
 *   no crash. Removing the offending favorite makes export work again.
 * - `favorites-export-status`: polite region inside `<main>`
 *   (`aria-live="polite"` or `role="status"`, never assertive/alert).
 *   Copy success matches /copied/i and never claims a saved file or a
 *   download. Download initiation matches /download/i and never claims
 *   the file was saved or that anything was copied. Failures never claim
 *   a copy, a download, or a saved file (claim-shaped wording, not a bare
 *   pointer to the browser's saved-downloads area).
 * - `favorites-export-fallback`: LABELED readonly <input>/<textarea>
 *   holding the exact numbered text, shown only when the Clipboard API
 *   is absent or rejects; focusable and fully selectable.
 *   `favorites-export-instructions`: manual-copy instructions naming a
 *   gesture, never claiming success. No `document.execCommand`.
 * - Exporting is read-only: favorites (stored + listed), queue order and
 *   current track, checkbox selection, playback, and Web Storage are
 *   unchanged; only explicit activation touches the clipboard or starts
 *   a download; at most one live Blob URL is retained at any time.
 *
 * RED NOW: none of the favorites-export hooks exist, so every case fails
 * at its button/status assertion with a NEXT action. Assertions about
 * announcements are scoped to the section/`main` regions, never
 * page-wide live-region counts.
 */

const ITEMS = [
	{ id: 'fav-z', title: 'Zulu Song', artist: 'Harbor Lights' },
	{ id: 'fav-a', title: 'Alpha Song', artist: 'Cedar' },
	{
		id: 'fav-m',
		title: 'Mike Song',
		artist: 'Harbor Lights',
		streamUrl: 'https://stream.example/mike?token=SEKRET_TOKEN'
	}
];
const CONTEXT = 'favorites-export-account';
const CONTEXT_KEY = 'nntune.library.auth-context.v1';
const KEY_BASE = 'nntune.library.favorites.v1';
const KEY = `${KEY_BASE}.${CONTEXT}`;
const ALL = ['fav-z', 'fav-a', 'fav-m'];

const DEFAULT_TEXT =
	'1. Zulu Song — Harbor Lights\n2. Alpha Song — Cedar\n3. Mike Song — Harbor Lights';
const DEFAULT_M3U =
	'#EXTM3U\n' +
	'#EXTINF:-1,Zulu Song — Harbor Lights\nfav-z\n' +
	'#EXTINF:-1,Alpha Song — Cedar\nfav-a\n' +
	'#EXTINF:-1,Mike Song — Harbor Lights\nfav-m';
// After moving Mike up once: arranged order fav-z, fav-m, fav-a. Distinct
// from load order (z/a/m), title order (a/m/z) and any queue built below.
const ARRANGED_TEXT =
	'1. Zulu Song — Harbor Lights\n2. Mike Song — Harbor Lights\n3. Alpha Song — Cedar';
const ARRANGED_M3U =
	'#EXTM3U\n' +
	'#EXTINF:-1,Zulu Song — Harbor Lights\nfav-z\n' +
	'#EXTINF:-1,Mike Song — Harbor Lights\nfav-m\n' +
	'#EXTINF:-1,Alpha Song — Cedar\nfav-a';
// Then moving Alpha up once: fav-z, fav-a, fav-m (same ids as load order
// but reached live, so a stale first-export snapshot still fails).
const REARRANGED_M3U = DEFAULT_M3U;

// Claim-shaped success wording only: an honest pointer to the browser's
// saved-downloads area, or "nothing was saved", must not trip these.
const FALSE_SUCCESS = /copied|downloaded|download started|successfully saved|saved successfully/i;
const SAVED_CLAIM =
	/(?:file|favorites?) (?:was|has been|is) saved|saved (?:to|as) (?:your|the|a) |successfully saved|saved successfully/i;
const EXPORT_ERROR =
	/could not|cannot|can't|unable|failed|error|unsafe|not available|isn't available|not supported|refused/i;
const LEAK_TOKENS = ['stream.example', 'SEKRET_TOKEN', 'https://', 'token='];

type ClipboardMode = 'fulfilled' | 'rejected' | 'missing';
type LibraryRow = { id: string; title?: string; artist?: string; streamUrl?: string };

function section(page: Page): Locator {
	return page.getByTestId('favorites-section');
}
function move(page: Page, title: string, direction: 'up' | 'down'): Locator {
	return section(page).getByRole('button', {
		name: `Move favorite ${title} ${direction}`,
		exact: true
	});
}
function copyButton(page: Page): Locator {
	return section(page).getByTestId('favorites-export-copy');
}
function downloadButton(page: Page): Locator {
	return section(page).getByTestId('favorites-export-download');
}
function statusRegion(page: Page): Locator {
	return section(page).getByTestId('favorites-export-status');
}
function star(page: Page, id: string): Locator {
	return page
		.locator(`[data-testid="library-item"][data-id="${id}"]`)
		.getByRole('button', { name: /^(?:Unstar|Star) / });
}

async function expectFavoriteIds(page: Page, ids: string[]): Promise<void> {
	await expect
		.poll(() =>
			page
				.getByTestId('favorite-item')
				.evaluateAll((nodes) => nodes.map((node) => node.getAttribute('data-id')))
		)
		.toEqual(ids);
}
async function stored(page: Page): Promise<unknown> {
	return page.evaluate((key) => JSON.parse(localStorage.getItem(key) ?? 'null'), KEY);
}
async function clipboardCalls(page: Page): Promise<string[]> {
	return page.evaluate(
		() => (window as unknown as { __favExportCalls: string[] }).__favExportCalls ?? []
	);
}
async function execCommandCallCount(page: Page): Promise<number> {
	return page.evaluate(
		() => ((window as unknown as { __execCommandCalls: unknown[] }).__execCommandCalls ?? []).length
	);
}
async function blobCounts(page: Page): Promise<{ created: number; live: number }> {
	return page.evaluate(() => {
		const tracker = (window as unknown as {
			__favBlob?: { created: number; live: string[] };
		}).__favBlob;
		return { created: tracker?.created ?? 0, live: tracker?.live.length ?? 0 };
	});
}
async function mediaCallCounts(page: Page): Promise<{ play: number; pause: number }> {
	return page.evaluate(
		() =>
			(window as unknown as { __mediaCalls: { play: number; pause: number } }).__mediaCalls ?? {
				play: 0,
				pause: 0
			}
	);
}
async function storageSnapshot(page: Page) {
	return page.evaluate(() => {
		const read = (storage: Storage) => {
			const out: Record<string, string | null> = {};
			try {
				for (let i = 0; i < storage.length; i += 1) {
					const key = storage.key(i);
					if (key !== null) out[key] = storage.getItem(key);
				}
			} catch {
				// Blocked storage reads as empty.
			}
			return out;
		};
		return { local: read(localStorage), session: read(sessionStorage) };
	});
}

async function open(
	page: Page,
	ids: string[],
	options: { items?: LibraryRow[]; clipboard?: ClipboardMode } = {}
): Promise<void> {
	const { items = ITEMS, clipboard = 'fulfilled' } = options;
	await page.addInitScript(
		({ contextKey, context, key, ids, mode }) => {
			// Seed only the first document, never overwrite on reload.
			if (!sessionStorage.getItem('test.favorites-export.seeded')) {
				sessionStorage.setItem('test.favorites-export.seeded', '1');
				sessionStorage.setItem(contextKey, context);
				localStorage.setItem(key, JSON.stringify(ids));
			}
			const calls: string[] = [];
			(window as unknown as { __favExportCalls: string[] }).__favExportCalls = calls;
			const writeText = (text: string) => {
				calls.push(text);
				if (mode === 'rejected')
					return Promise.reject(new DOMException('Denied', 'NotAllowedError'));
				return Promise.resolve();
			};
			if (mode === 'missing') {
				Object.defineProperty(navigator, 'clipboard', { value: undefined, configurable: true });
			} else {
				Object.defineProperty(navigator, 'clipboard', {
					value: { writeText },
					configurable: true
				});
			}
			const execCalls: unknown[][] = [];
			(window as unknown as { __execCommandCalls: unknown[][] }).__execCommandCalls = execCalls;
			(document as unknown as { execCommand: (...args: unknown[]) => boolean }).execCommand = (
				...args: unknown[]
			) => {
				execCalls.push(args);
				return false;
			};
			const tracker = { created: 0, live: [] as string[] };
			(window as unknown as { __favBlob: typeof tracker }).__favBlob = tracker;
			const createObjectURL = URL.createObjectURL.bind(URL);
			const revokeObjectURL = URL.revokeObjectURL.bind(URL);
			URL.createObjectURL = (object: Blob | MediaSource) => {
				const url = createObjectURL(object);
				tracker.created += 1;
				tracker.live.push(url);
				return url;
			};
			URL.revokeObjectURL = (url: string) => {
				tracker.live = tracker.live.filter((candidate) => candidate !== url);
				revokeObjectURL(url);
			};
			const mediaCalls = { play: 0, pause: 0 };
			(window as unknown as { __mediaCalls: typeof mediaCalls }).__mediaCalls = mediaCalls;
			HTMLMediaElement.prototype.play = function () {
				mediaCalls.play += 1;
				return new Promise<void>(() => {});
			};
			HTMLMediaElement.prototype.pause = function () {
				mediaCalls.pause += 1;
			};
		},
		{ contextKey: CONTEXT_KEY, context: CONTEXT, key: KEY, ids, mode: clipboard }
	);
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(items)
		})
	);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(items.length);
	await expect(section(page)).toBeVisible();
	await expectFavoriteIds(
		page,
		ids.filter((id) => items.some((item) => item.id === id))
	);
}

async function expectExportButtons(page: Page, caseId: string) {
	const copy = copyButton(page);
	await expect(
		copy,
		`${caseId}: expected a labeled native Copy button in the Favorites section (actual: missing). NEXT: Builder adds <button data-testid="favorites-export-copy">Copy favorites as text</button> to the library Favorites section.`
	).toBeVisible();
	await expect(copy, `${caseId}: copy must be a native <button>.`).toHaveJSProperty(
		'tagName',
		'BUTTON'
	);
	await expect(copy, `${caseId}: copy accessible name must be exactly the locked label.`).toHaveAccessibleName(
		/^Copy favorites as text$/
	);
	const download = downloadButton(page);
	await expect(
		download,
		`${caseId}: expected a labeled native Download button in the Favorites section (actual: missing). NEXT: Builder adds <button data-testid="favorites-export-download">Download favorites (.m3u)</button> to the library Favorites section.`
	).toBeVisible();
	await expect(download, `${caseId}: download must be a native <button>.`).toHaveJSProperty(
		'tagName',
		'BUTTON'
	);
	await expect(
		download,
		`${caseId}: download accessible name must be exactly the locked label.`
	).toHaveAccessibleName(/^Download favorites \(\.m3u\)$/);
	return { copy, download };
}

const audits = new WeakMap<Page, { errors: string[]; mutations: string[]; downloads: Download[] }>();

function downloadsOf(page: Page): Download[] {
	return audits.get(page)!.downloads;
}

async function downloadBody(
	page: Page,
	index: number,
	testInfo: { outputPath: (...pathSegments: string[]) => string }
): Promise<{ filename: string; body: string }> {
	await expect.poll(() => downloadsOf(page).length).toBeGreaterThan(index);
	const file = downloadsOf(page)[index]!;
	const saved = testInfo.outputPath(`favorites-${index}.m3u`);
	await mkdir(dirname(saved), { recursive: true });
	await file.saveAs(saved);
	return { filename: file.suggestedFilename(), body: await readFile(saved, 'utf-8') };
}

async function expectStatusPoliteInMain(page: Page, caseId: string): Promise<Locator> {
	const inMain = page.locator('main').getByTestId('favorites-export-status');
	await expect(
		inMain,
		`${caseId}: the export status region must live inside <main> (announcements scoped to main).`
	).toHaveCount(1);
	const status = statusRegion(page);
	const politeness = await status.evaluate((el) => ({
		live: el.getAttribute('aria-live'),
		role: el.getAttribute('role')
	}));
	expect(
		politeness.live === 'polite' || politeness.role === 'status',
		`${caseId}: status must be polite (actual aria-live=${politeness.live} role=${politeness.role})`
	).toBe(true);
	expect(politeness.live, `${caseId}: status must never be assertive`).not.toBe('assertive');
	expect(politeness.role, `${caseId}: status must never be an alert`).not.toBe('alert');
	return status;
}

/** Absent, hidden, or disabled all satisfy "not offered"; any button that
 * is still in the DOM must additionally stay inert under a programmatic
 * click (callers assert no clipboard write / download followed). */
async function expectActionUnavailable(button: Locator, caseId: string, label: string): Promise<void> {
	if ((await button.count()) === 0) return;
	if (await button.isVisible()) {
		await expect(
			button,
			`${caseId}: ${label} must be truthfully disabled while no favorite is listed (or absent/hidden). NEXT: Builder hides or disables both export actions for an empty Favorites section.`
		).toBeDisabled();
	}
	await button.evaluate((node: HTMLButtonElement) => node.click());
}

async function expectNoFallbackShown(page: Page): Promise<void> {
	const fallback = section(page).getByTestId('favorites-export-fallback');
	if ((await fallback.count()) > 0) {
		await expect(fallback, 'no fallback may be shown on this path').toBeHidden();
	}
}

test.beforeEach(async ({ page }) => {
	const audit = { errors: [] as string[], mutations: [] as string[], downloads: [] as Download[] };
	audits.set(page, audit);
	page.on('pageerror', (error) => audit.errors.push(error.message));
	page.on('download', (download) => audit.downloads.push(download));
	page.on('request', (request) => {
		if (request.method() !== 'GET')
			audit.mutations.push(`${request.method()} ${new URL(request.url()).pathname}`);
	});
	// No live backend, including unexpected mutations or external providers.
	await page.context().route('**/*', (route) => {
		const url = new URL(route.request().url());
		if (!['localhost', '127.0.0.1'].includes(url.hostname)) return route.abort('blockedbyclient');
		if (/^\/(?:api|v1)(?:\/|$)/.test(url.pathname)) return route.abort('blockedbyclient');
		return route.continue();
	});
});

test.afterEach(async ({ page }) => {
	const audit = audits.get(page)!;
	expect.soft(audit.errors, 'favorites export must not cause an uncaught browser error').toEqual([]);
	expect.soft(audit.mutations, 'favorites export must remain device-local').toEqual([]);
});

test('[FAVEXPORT-01] export actions are labeled native buttons, keyboard-operable, at least 24x24 on a narrow screen', async ({
	page
}, testInfo) => {
	await page.setViewportSize({ width: 360, height: 780 });
	await open(page, ALL);
	const { copy, download } = await expectExportButtons(page, 'FAVEXPORT-01');

	for (const [label, button] of [
		['copy', copy],
		['download', download]
	] as const) {
		const box = await button.boundingBox();
		expect(box, `${label} must have a bounding box`).not.toBeNull();
		expect(box!.width, `${label} target must be at least 24px wide`).toBeGreaterThanOrEqual(24);
		expect(box!.height, `${label} target must be at least 24px tall`).toBeGreaterThanOrEqual(24);
		expect(box!.x, `${label} must stay inside the 360px viewport`).toBeGreaterThanOrEqual(0);
		expect(box!.x + box!.width, `${label} must stay inside the 360px viewport`).toBeLessThanOrEqual(
			360
		);
	}

	// Reach both via actual Tab navigation rather than a focus shortcut.
	const found = new Set<string>();
	await page.getByRole('combobox', { name: 'Appearance', exact: true }).focus();
	for (let tab = 0; tab < 100 && found.size < 2; tab += 1) {
		await page.keyboard.press('Tab');
		const testid = await page.evaluate(
			() => (document.activeElement as HTMLElement | null)?.getAttribute('data-testid') ?? null
		);
		if (testid === 'favorites-export-copy' || testid === 'favorites-export-download') {
			found.add(testid);
		}
	}
	expect([...found].sort(), 'FAVEXPORT-01: both export actions must be reachable by Tab').toEqual([
		'favorites-export-copy',
		'favorites-export-download'
	]);

	await copy.focus();
	await page.keyboard.press('Enter');
	await expect
		.poll(() => clipboardCalls(page), 'Enter on the copy button must write the clipboard')
		.toEqual([DEFAULT_TEXT]);

	await download.focus();
	await page.keyboard.press('Space');
	const { filename, body } = await downloadBody(page, 0, testInfo);
	expect(filename, 'keyboard-activated download must be named favorites.m3u').toBe('favorites.m3u');
	expect(body, 'keyboard-activated download must carry the favorites M3U').toBe(DEFAULT_M3U);
});

test('[FAVEXPORT-02] copy writes the numbered text in arranged order, not load, title, or queue order', async ({
	page
}) => {
	await open(page, ALL);
	const { copy } = await expectExportButtons(page, 'FAVEXPORT-02');

	// Diverge every other order a wrong implementation might reuse:
	// main list by title (a/m/z), queue built Alpha then Zulu (a/z),
	// favorites arranged by moving Mike up (z/m/a).
	await page.getByRole('combobox', { name: 'Order tracks', exact: true }).selectOption('title');
	await page
		.locator('[data-testid="library-item"][data-id="fav-a"]')
		.getByRole('button', { name: /^Alpha Song/ })
		.click();
	await page
		.locator('[data-testid="library-item"][data-id="fav-z"]')
		.getByRole('button', { name: /^Zulu Song/ })
		.click();
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	await move(page, 'Mike Song', 'up').click();
	await expectFavoriteIds(page, ['fav-z', 'fav-m', 'fav-a']);

	// Only explicit activation touches the clipboard.
	expect(await clipboardCalls(page), 'no clipboard write before activation').toEqual([]);

	await copy.click();
	await expect
		.poll(() => clipboardCalls(page), 'copy must write exactly the arranged numbered text')
		.toEqual([ARRANGED_TEXT]);
	const copied = (await clipboardCalls(page))[0] ?? '';
	for (const leaked of LEAK_TOKENS) {
		expect(copied, `copied text must not contain "${leaked}"`).not.toContain(leaked);
	}

	const status = await expectStatusPoliteInMain(page, 'FAVEXPORT-02');
	await expect(status, 'copy success must be announced').toContainText(/copied/i);
	await expect(status, 'copy success must not claim a saved file').not.toContainText(SAVED_CLAIM);
	await expect(status, 'copy success must not claim a download').not.toContainText(/download/i);
	await expectNoFallbackShown(page);
	expect(downloadsOf(page), 'copy must not start a download').toEqual([]);
});

test('[FAVEXPORT-03] download saves favorites.m3u in arranged order, reads live order on repeat, retains at most one Blob URL', async ({
	page
}, testInfo) => {
	await open(page, ALL);
	const { download } = await expectExportButtons(page, 'FAVEXPORT-03');
	const baseline = await blobCounts(page);

	await move(page, 'Mike Song', 'up').click();
	await expectFavoriteIds(page, ['fav-z', 'fav-m', 'fav-a']);
	expect(await blobCounts(page), 'arranging must not create a Blob URL').toEqual(baseline);

	await download.click();
	const first = await downloadBody(page, 0, testInfo);
	expect(first.filename, 'download must be named favorites.m3u').toBe('favorites.m3u');
	expect(first.body, 'downloaded bytes must be the arranged favorites M3U').toBe(ARRANGED_M3U);
	for (const leaked of LEAK_TOKENS) {
		expect(first.body, `M3U bytes must not contain "${leaked}"`).not.toContain(leaked);
	}
	expect(first.body, 'M3U bytes carry no control characters beyond line breaks').not.toMatch(
		/[\0-\x09\x0B-\x1F\x7F]/
	);

	const status = await expectStatusPoliteInMain(page, 'FAVEXPORT-03');
	await expect(status, 'download initiation must be announced').toContainText(/download/i);
	await expect(status, 'download must not claim the file was saved').not.toContainText(SAVED_CLAIM);
	await expect(status, 'download must not claim a copy happened').not.toContainText(/copied/i);
	expect(await clipboardCalls(page), 'download must not write the clipboard').toEqual([]);
	await expect
		.poll(async () => (await blobCounts(page)).live)
		.toBeLessThanOrEqual(baseline.live + 1);

	// A second activation reads the live arranged order, and the first
	// Blob URL is released: still at most one retained overall.
	await move(page, 'Alpha Song', 'up').click();
	await expectFavoriteIds(page, ['fav-z', 'fav-a', 'fav-m']);
	await download.click();
	const second = await downloadBody(page, 1, testInfo);
	expect(second.filename, 'second download must be named favorites.m3u').toBe('favorites.m3u');
	expect(second.body, 'second download must read the live arranged order').toBe(REARRANGED_M3U);
	const after = await blobCounts(page);
	expect(after.created, 'no more than one Blob URL may be created per download').toBeLessThanOrEqual(
		baseline.created + 2
	);
	expect(after.live, 'at most one live Blob URL is retained').toBeLessThanOrEqual(baseline.live + 1);
	expect(await execCommandCallCount(page), 'download must not use execCommand').toBe(0);
});

test('[FAVEXPORT-04] exporting mutates neither favorites, queue, selection, playback, nor storage', async ({
	page
}) => {
	await open(page, ALL);
	const { copy, download } = await expectExportButtons(page, 'FAVEXPORT-04');

	await page
		.locator('[data-testid="library-item"][data-id="fav-a"]')
		.getByRole('button', { name: /^Alpha Song/ })
		.click();
	await page
		.locator('[data-testid="library-item"][data-id="fav-z"]')
		.getByRole('button', { name: /^Zulu Song/ })
		.click();
	await page.getByRole('checkbox', { name: 'Select Mike Song', exact: true }).check();
	await move(page, 'Mike Song', 'up').click();
	await expectFavoriteIds(page, ['fav-z', 'fav-m', 'fav-a']);
	expect(await stored(page)).toEqual(['fav-z', 'fav-m', 'fav-a']);

	const before = {
		stored: await stored(page),
		libraryIds: await page
			.getByTestId('library-item')
			.evaluateAll((nodes) => nodes.map((node) => node.getAttribute('data-id'))),
		queueIds: await page
			.getByTestId('queue-item')
			.evaluateAll((nodes) => nodes.map((node) => node.getAttribute('data-track-id'))),
		nowPlaying: await page.getByTestId('now-playing').textContent(),
		selectionCount: await page.getByTestId('selection-count').textContent(),
		mikeSelected: await page
			.getByRole('checkbox', { name: 'Select Mike Song', exact: true })
			.isChecked(),
		audioCount: await page.getByTestId('audio-player').count(),
		media: await mediaCallCounts(page),
		storage: await storageSnapshot(page)
	};

	await copy.click();
	await expect.poll(() => clipboardCalls(page)).toEqual([ARRANGED_TEXT]);
	await download.click();
	await expect.poll(() => downloadsOf(page).length).toBe(1);

	expect(await stored(page), 'stored favorites must not change').toEqual(before.stored);
	await expectFavoriteIds(page, ['fav-z', 'fav-m', 'fav-a']);
	expect(
		await page
			.getByTestId('library-item')
			.evaluateAll((nodes) => nodes.map((node) => node.getAttribute('data-id'))),
		'library order must not change'
	).toEqual(before.libraryIds);
	expect(
		await page
			.getByTestId('queue-item')
			.evaluateAll((nodes) => nodes.map((node) => node.getAttribute('data-track-id'))),
		'queue order must not change'
	).toEqual(before.queueIds);
	expect(await page.getByTestId('now-playing').textContent(), 'current track must not change').toBe(
		before.nowPlaying
	);
	expect(
		await page.getByTestId('selection-count').textContent(),
		'selection count must not change'
	).toBe(before.selectionCount);
	expect(
		await page.getByRole('checkbox', { name: 'Select Mike Song', exact: true }).isChecked(),
		'checkbox selection must not change'
	).toBe(before.mikeSelected);
	expect(await page.getByTestId('audio-player').count(), 'playback element must not appear/vanish').toBe(
		before.audioCount
	);
	expect(await mediaCallCounts(page), 'export must not play or pause').toEqual(before.media);
	expect(await storageSnapshot(page), 'export must not persist anything').toEqual(before.storage);
});

for (const [label, seed] of [
	['no stored favorites', [] as string[]],
	['only absent stored ids', ['gone-only'] as string[]]
] as const) {
	test(`[FAVEXPORT-05:${label}] empty favorites offer no enabled export and claim no success`, async ({
		page
	}) => {
		await open(page, seed);
		await expect(page.getByTestId('favorites-empty')).toBeVisible();

		for (const testid of ['favorites-export-copy', 'favorites-export-download'] as const) {
			await expectActionUnavailable(section(page).getByTestId(testid), 'FAVEXPORT-05', testid);
		}

		expect(await clipboardCalls(page), 'empty export must not write the clipboard').toEqual([]);
		expect(downloadsOf(page), 'empty export must not deliver an empty file as success').toEqual([]);
		const status = statusRegion(page);
		if ((await status.count()) > 0) {
			await expect(status, 'empty state must not claim a successful export').not.toContainText(
				FALSE_SUCCESS
			);
		}
		await expectNoFallbackShown(page);

		// Non-vacuous: the same page must offer enabled actions as soon as a
		// favorite exists (this is what fails before the feature exists),
		// and withdraw them again when it is removed.
		await star(page, 'fav-m').click();
		await expectFavoriteIds(page, ['fav-m']);
		const { copy, download } = await expectExportButtons(page, 'FAVEXPORT-05');
		await expect(copy, 'copy must enable once a favorite is listed').toBeEnabled();
		await expect(download, 'download must enable once a favorite is listed').toBeEnabled();

		await star(page, 'fav-m').click();
		await expectFavoriteIds(page, []);
		await expectActionUnavailable(copy, 'FAVEXPORT-05', 'copy');
		await expectActionUnavailable(download, 'FAVEXPORT-05', 'download');
		expect(await clipboardCalls(page), 'still no clipboard write without activation').toEqual([]);
		expect(downloadsOf(page), 'still no download without activation').toEqual([]);
	});
}

test('[FAVEXPORT-06] unstarring the last favorite withdraws or disables both export actions', async ({
	page
}) => {
	await open(page, ['fav-m']);
	const { copy, download } = await expectExportButtons(page, 'FAVEXPORT-06');
	await expect(copy).toBeEnabled();
	await expect(download).toBeEnabled();

	await section(page).getByRole('button', { name: 'Unstar Mike Song', exact: true }).click();
	await expectFavoriteIds(page, []);
	await expect(page.getByTestId('favorites-empty')).toBeVisible();

	for (const [label, button] of [
		['copy', copy],
		['download', download]
	] as const) {
		await expectActionUnavailable(button, 'FAVEXPORT-06', label);
	}
	expect(await clipboardCalls(page), 'no clipboard write after emptying').toEqual([]);
	expect(downloadsOf(page), 'no download after emptying').toEqual([]);
	const status = statusRegion(page);
	if ((await status.count()) > 0) {
		await expect(status, 'emptying must not leave a success claim').not.toContainText(FALSE_SUCCESS);
	}
});

test('[FAVEXPORT-07] an export-unsafe favorite id refuses both actions truthfully, then recovers once removed', async ({
	page
}) => {
	const items: LibraryRow[] = [
		{ id: 'fav-z', title: 'Zulu Song', artist: 'Harbor Lights' },
		{ id: 'bad/id', title: 'Bad Slash', artist: 'Guard' }
	];
	await open(page, ['fav-z', 'bad/id'], { items });
	const { copy, download } = await expectExportButtons(page, 'FAVEXPORT-07');

	await download.click();
	const status = await expectStatusPoliteInMain(page, 'FAVEXPORT-07');
	await expect(
		status,
		'unsafe id must surface a truthful in-page error for the download'
	).toContainText(EXPORT_ERROR);
	await expect(status, 'refused download must not claim success').not.toContainText(FALSE_SUCCESS);
	expect(downloadsOf(page), 'refused download must not deliver a partial file').toEqual([]);
	await expectNoFallbackShown(page);

	await copy.click();
	await expect(
		status,
		'unsafe id must surface a truthful in-page error for the copy'
	).toContainText(EXPORT_ERROR);
	await expect(status, 'refused copy must not claim success').not.toContainText(FALSE_SUCCESS);
	expect(await clipboardCalls(page), 'refused copy must not write the clipboard').toEqual([]);
	await expectNoFallbackShown(page);

	// The refusal changed nothing and the page is still fully usable.
	expect(await stored(page), 'stored favorites must survive the refusal').toEqual([
		'fav-z',
		'bad/id'
	]);
	await expectFavoriteIds(page, ['fav-z', 'bad/id']);
	expect(await page.getByTestId('library-item').count()).toBe(2);

	await section(page).getByRole('button', { name: 'Unstar Bad Slash', exact: true }).click();
	await expectFavoriteIds(page, ['fav-z']);
	await copy.click();
	await expect
		.poll(() => clipboardCalls(page), 'export must work again once the unsafe favorite is removed')
		.toEqual(['1. Zulu Song — Harbor Lights']);
	await expect(status, 'recovered copy may now truthfully announce success').toContainText(
		/copied/i
	);
});

async function expectFallback(page: Page, caseId: string, expectedText: string): Promise<void> {
	const fallback = section(page).getByTestId('favorites-export-fallback');
	await expect(
		fallback,
		`${caseId}: expected selectable fallback text when the clipboard cannot be used (actual: missing). NEXT: Builder shows a labeled readonly favorites-export-fallback holding the exact numbered text.`
	).toBeVisible();
	const tag = await fallback.evaluate((el) => el.tagName);
	expect(
		tag === 'INPUT' || tag === 'TEXTAREA',
		`${caseId}: fallback must be input/textarea (actual ${tag})`
	).toBe(true);
	await expect(fallback, `${caseId}: fallback must be labeled`).toHaveAccessibleName(/.+/);
	expect(
		await fallback.evaluate((el) => (el as HTMLInputElement).readOnly),
		`${caseId}: fallback must be readonly`
	).toBe(true);
	expect(await fallback.inputValue(), `${caseId}: fallback must hold the exact arranged text`).toBe(
		expectedText
	);

	const instructions = section(page).getByTestId('favorites-export-instructions');
	await expect(instructions, `${caseId}: fallback must come with manual-copy instructions`).toBeVisible();
	await expect(instructions).toContainText(/copy/i);
	await expect(instructions, `${caseId}: instructions must name an actionable gesture`).toContainText(
		/ctrl|cmd|⌘|right-click|long-press|keyboard|select/i
	);
	await expect(instructions, `${caseId}: instructions must not claim success`).not.toContainText(
		FALSE_SUCCESS
	);

	const status = statusRegion(page);
	if ((await status.count()) > 0) {
		await expect(status, `${caseId}: a refused copy must not announce success`).not.toContainText(
			FALSE_SUCCESS
		);
	}
	expect(await execCommandCallCount(page), `${caseId}: fallback must not use execCommand`).toBe(0);

	await fallback.focus();
	await fallback.evaluate((el) => (el as HTMLInputElement).select());
	const selection = await fallback.evaluate((el) => ({
		start: (el as HTMLInputElement).selectionStart,
		end: (el as HTMLInputElement).selectionEnd,
		length: (el as HTMLInputElement).value.length
	}));
	expect(selection.start, `${caseId}: fallback must select from the start`).toBe(0);
	expect(selection.end, `${caseId}: fallback must select through the end`).toBe(selection.length);
}

test('[FAVEXPORT-08] a rejected clipboard write leaves selectable arranged fallback text and no false success', async ({
	page
}) => {
	await open(page, ALL, { clipboard: 'rejected' });
	const { copy } = await expectExportButtons(page, 'FAVEXPORT-08');
	await move(page, 'Mike Song', 'up').click();
	await expectFavoriteIds(page, ['fav-z', 'fav-m', 'fav-a']);

	await copy.click();
	await expectFallback(page, 'FAVEXPORT-08', ARRANGED_TEXT);
	await expect(copy, 'copy stays enabled after a rejected write').toBeEnabled();
	expect(downloadsOf(page), 'a refused copy must not fall back to a download').toEqual([]);
});

test('[FAVEXPORT-09] a missing Clipboard API leaves selectable arranged fallback text and no false success', async ({
	page
}) => {
	await open(page, ALL, { clipboard: 'missing' });
	const { copy } = await expectExportButtons(page, 'FAVEXPORT-09');

	await copy.click();
	await expectFallback(page, 'FAVEXPORT-09', DEFAULT_TEXT);
	expect(await clipboardCalls(page), 'no clipboard API means no recorded write').toEqual([]);
});

test('[FAVEXPORT-10] labels equal the shared formatting on blank/control/unicode fields and absent ids are skipped but kept', async ({
	page
}, testInfo) => {
	const items: LibraryRow[] = [
		{ id: 'lbl-unicode', title: 'Björk 🦢', artist: 'Sigur Rós 日本語' },
		{ id: 'lbl-solo', title: 'Solo Flight', artist: '   ' },
		{ id: 'lbl-blank-title', title: '   ', artist: 'Ghost Choir' },
		{ id: 'lbl-ctrl', title: 'One\nTwo\tThree', artist: 'A\0B' }
	];
	const storedSeed = [
		'gone-first',
		'lbl-ctrl',
		'lbl-blank-title',
		'gone-mid',
		'lbl-solo',
		'lbl-unicode'
	];
	const EDGE_TEXT =
		'1. One Two Three — A B\n' +
		'2. lbl-blank-title — Ghost Choir\n' +
		'3. Solo Flight\n' +
		'4. Björk 🦢 — Sigur Rós 日本語';
	const EDGE_M3U =
		'#EXTM3U\n' +
		'#EXTINF:-1,One Two Three — A B\nlbl-ctrl\n' +
		'#EXTINF:-1,lbl-blank-title — Ghost Choir\nlbl-blank-title\n' +
		'#EXTINF:-1,Solo Flight\nlbl-solo\n' +
		'#EXTINF:-1,Björk 🦢 — Sigur Rós 日本語\nlbl-unicode';

	await open(page, storedSeed, { items });
	const { copy, download } = await expectExportButtons(page, 'FAVEXPORT-10');
	await expectFavoriteIds(page, ['lbl-ctrl', 'lbl-blank-title', 'lbl-solo', 'lbl-unicode']);

	await copy.click();
	await expect
		.poll(() => clipboardCalls(page), 'copied text must equal the shared label formatting')
		.toEqual([EDGE_TEXT]);
	expect(
		(await clipboardCalls(page))[0] ?? '',
		'copied text carries no injected line breaks or control characters beyond its line separators'
	).not.toMatch(/[\0-\x09\x0B-\x1F\x7F]/);

	await download.click();
	const { filename, body } = await downloadBody(page, 0, testInfo);
	expect(filename).toBe('favorites.m3u');
	expect(body, 'M3U must equal the shared label formatting, skipping absent ids').toBe(EDGE_M3U);
	expect(body).not.toContain('gone-');

	expect(await stored(page), 'absent ids stay stored after exporting').toEqual(storedSeed);
});
