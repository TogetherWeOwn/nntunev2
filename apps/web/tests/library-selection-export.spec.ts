import { expect, test, type Download, type Locator, type Page } from '@playwright/test';
import { mkdir, readFile } from 'node:fs/promises';
import { dirname } from 'node:path';

/**
 * Test-Author owned RED acceptance for SELECTEXPORT: export the selected
 * loaded tracks from the library page selection UI, in checkbox
 * activation order, as a downloaded M3U or as copied text. The Builder
 * wires the library page to satisfy this file plus
 * `apps/web/src/lib/library/selectionExport.acceptance.test.ts` and must
 * not edit either file. Real library page, scoped browser storage and
 * queue; only HTTP is mocked. No proposed-helper imports: RED must be a
 * browser assertion, not a missing API.
 *
 * LOCKED PAGE CONTRACT (selection UI of `/library`):
 * - `selection-export-copy`: NATIVE <button> with accessible name exactly
 *   "Copy selected as text". `selection-export-download`: NATIVE <button>
 *   with accessible name exactly "Download selected (.m3u)". Both live
 *   with the existing selection actions inside `<main>`, are keyboard
 *   reachable and operable (Enter/Space), and are at least 24x24 CSS px.
 *   While the selection is empty they are absent or truthfully disabled —
 *   never an enabled action that produces an empty "successful" export.
 * - Bytes come from the single shared queue formatting core
 *   (`formatQueueText` / `formatQueueM3U`) over exactly the selected
 *   loaded tracks, resolved in ACTIVATION order (the order the boxes
 *   were checked) at activation time — never loaded order, never the
 *   Selected-only display order, never find/ordering-shaped visible
 *   order, and never queue order. Selected tracks hidden by the find
 *   field or by the current view are still selected loaded tracks and
 *   are exported. Unchecking then re-checking a row moves it to the end
 *   of the next export. Numbered `N. <title>[ — <artist>]` lines for
 *   copy; `#EXTM3U` + `#EXTINF:-1,<label>` + bare stable id lines,
 *   downloaded as `selected.m3u`. Never stream URLs, tokens, provider
 *   info, or identity.
 * - A selected track whose id the shared guard refuses makes BOTH
 *   actions refuse truthfully: an in-page error, no clipboard write, no
 *   download (not even a partial one), no false "Copied"/"Downloaded",
 *   no crash. Deselecting the offending track makes export work again.
 * - `selection-export-status`: polite region inside `<main>`
 *   (`aria-live="polite"` or `role="status"`, never assertive/alert).
 *   Copy success matches /copied/i and never claims a saved file or a
 *   download. Download initiation matches /download/i and never claims
 *   the file was saved or that anything was copied. Failures never claim
 *   a copy, a download, or a saved file (claim-shaped wording, not a bare
 *   pointer to the browser's saved-downloads area).
 * - `selection-export-fallback`: LABELED readonly <input>/<textarea>
 *   holding the exact numbered text, shown only when the Clipboard API
 *   is absent or rejects; focusable and fully selectable.
 *   `selection-export-instructions`: manual-copy instructions naming a
 *   gesture, never claiming success. No `document.execCommand`.
 * - Exporting is read-only: the checkbox selection, favorites (starred
 *   and listed), queue order and current track, playback, the current
 *   view/find/ordering, and Web Storage are unchanged; only explicit
 *   activation touches the clipboard or starts a download; at most one
 *   live Blob URL is retained at any time.
 *
 * RED NOW: none of the selection-export hooks exist, so every case fails
 * at its button/status assertion with a NEXT action. Assertions about
 * announcements are scoped to `main` regions, never page-wide
 * live-region counts.
 *
 * Focused command (repo root, deterministic, offline):
 *   CI=1 PUBLIC_API_BASE_URL='' pnpm --dir apps/web exec playwright test \
 *     tests/library-selection-export.spec.ts --workers=1 --retries=0
 */

// Loaded (API) order is z/a/m/b/d on purpose: activation order in these
// cases is d/z/m, loaded order of that subset is z/m/d, title order is
// d/m/z, so bytes in any of those orders (or a queue's) cannot match.
const ITEMS = [
	{ id: 'sel-z', title: 'Zulu Song', artist: 'Harbor Lights' },
	{ id: 'sel-a', title: 'Alpha Song', artist: 'Cedar' },
	{
		id: 'sel-m',
		title: 'Mike Song',
		artist: 'Harbor Lights',
		streamUrl: 'https://stream.example/mike?token=SEKRET_TOKEN'
	},
	{ id: 'sel-b', title: 'Bravo Song', artist: 'Solo Act' },
	{ id: 'sel-d', title: 'Delta Song', artist: 'Echo Band' }
];
const ALL_IDS = ITEMS.map((item) => item.id);
const ACTIVATED = ['sel-d', 'sel-z', 'sel-m'];

const ACTIVATION_TEXT =
	'1. Delta Song — Echo Band\n2. Zulu Song — Harbor Lights\n3. Mike Song — Harbor Lights';
const ACTIVATION_M3U =
	'#EXTM3U\n' +
	'#EXTINF:-1,Delta Song — Echo Band\nsel-d\n' +
	'#EXTINF:-1,Zulu Song — Harbor Lights\nsel-z\n' +
	'#EXTINF:-1,Mike Song — Harbor Lights\nsel-m';
// After unchecking Zulu and re-checking it: activation order d/m/z. This
// happens to equal title order, but it is reached live from an export
// that was d/z/m, so a stale first-export snapshot still fails.
const REACTIVATED_M3U =
	'#EXTM3U\n' +
	'#EXTINF:-1,Delta Song — Echo Band\nsel-d\n' +
	'#EXTINF:-1,Mike Song — Harbor Lights\nsel-m\n' +
	'#EXTINF:-1,Zulu Song — Harbor Lights\nsel-z';

// Claim-shaped success wording only: an honest pointer to the browser's
// saved-downloads area, or "nothing was saved", must not trip these.
const FALSE_SUCCESS = /copied|downloaded|download started|successfully saved|saved successfully/i;
const SAVED_CLAIM =
	/(?:file|tracks?) (?:was|has been|is) saved|saved (?:to|as) (?:your|the|a) |successfully saved|saved successfully/i;
const EXPORT_ERROR =
	/could not|cannot|can't|unable|failed|error|unsafe|not available|isn't available|not supported|refused/i;
const LEAK_TOKENS = ['stream.example', 'SEKRET_TOKEN', 'https://', 'token='];

type ClipboardMode = 'fulfilled' | 'rejected' | 'missing';
type LibraryRow = { id: string; title?: string; artist?: string; streamUrl?: string };

function row(page: Page, id: string): Locator {
	return page.locator(`[data-testid="library-item"][data-id="${id}"]`);
}
function checkbox(page: Page, id: string): Locator {
	return row(page, id).getByRole('checkbox');
}
function showControl(page: Page): Locator {
	return page.getByRole('combobox', { name: 'Show', exact: true });
}
function orderControl(page: Page): Locator {
	return page.getByRole('combobox', { name: 'Order tracks', exact: true });
}
function findField(page: Page): Locator {
	return page.getByRole('searchbox', { name: 'Find in loaded tracks', exact: true });
}
function selectionCount(page: Page): Locator {
	return page.getByTestId('selection-count');
}
function copyButton(page: Page): Locator {
	return page.locator('main').getByTestId('selection-export-copy');
}
function downloadButton(page: Page): Locator {
	return page.locator('main').getByTestId('selection-export-download');
}
function statusRegion(page: Page): Locator {
	return page.locator('main').getByTestId('selection-export-status');
}

async function displayedIds(page: Page): Promise<(string | null)[]> {
	return page
		.getByTestId('library-item')
		.evaluateAll((rows) => rows.map((item) => item.getAttribute('data-id')));
}
async function expectDisplayed(page: Page, want: string[]): Promise<void> {
	await expect.poll(() => displayedIds(page)).toEqual(want);
}
async function queueOrder(page: Page): Promise<(string | null)[]> {
	return page
		.getByTestId('queue-item')
		.evaluateAll((rows) =>
			rows.map((item) => item.getAttribute('data-track-id') ?? item.getAttribute('data-id'))
		);
}
async function favoriteIds(page: Page): Promise<(string | null)[]> {
	return page
		.getByTestId('favorite-item')
		.evaluateAll((items) => items.map((item) => item.getAttribute('data-id')));
}
async function selectedStates(page: Page, ids: readonly string[]): Promise<boolean[]> {
	const states: boolean[] = [];
	for (const id of ids) states.push(await checkbox(page, id).isChecked());
	return states;
}
async function clipboardCalls(page: Page): Promise<string[]> {
	return page.evaluate(
		() => (window as unknown as { __selExportCalls: string[] }).__selExportCalls ?? []
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
			__selBlob?: { created: number; live: string[] };
		}).__selBlob;
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
	options: { items?: LibraryRow[]; clipboard?: ClipboardMode } = {}
): Promise<void> {
	const { items = ITEMS, clipboard = 'fulfilled' } = options;
	await page.addInitScript((mode: ClipboardMode) => {
		const calls: string[] = [];
		(window as unknown as { __selExportCalls: string[] }).__selExportCalls = calls;
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
		(window as unknown as { __selBlob: typeof tracker }).__selBlob = tracker;
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
	}, clipboard);
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(items)
		})
	);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(items.length);
	await expect(selectionCount(page)).toContainText('0 selected');
}

async function selectTracks(page: Page, activationOrder: readonly string[]): Promise<void> {
	for (const id of activationOrder) {
		const box = checkbox(page, id);
		await expect(box, `checkbox missing on row ${id}`).toBeVisible();
		await box.check();
	}
}

async function chooseShow(page: Page, label: 'All tracks' | 'Favorites only' | 'Selected only') {
	await expect(
		showControl(page).getByRole('option', { name: label, exact: true }),
		`Show control must offer "${label}"`
	).toHaveCount(1);
	await showControl(page).selectOption({ label });
	await expect(showControl(page).locator('option:checked')).toHaveText(label);
}

async function expectExportButtons(page: Page, caseId: string) {
	const copy = copyButton(page);
	await expect(
		copy,
		`${caseId}: expected a labeled native Copy button with the selection actions (actual: missing). NEXT: Builder adds <button data-testid="selection-export-copy">Copy selected as text</button> to the library selection UI.`
	).toBeVisible();
	await expect(copy, `${caseId}: copy must be a native <button>.`).toHaveJSProperty(
		'tagName',
		'BUTTON'
	);
	await expect(
		copy,
		`${caseId}: copy accessible name must be exactly the locked label.`
	).toHaveAccessibleName(/^Copy selected as text$/);
	const download = downloadButton(page);
	await expect(
		download,
		`${caseId}: expected a labeled native Download button with the selection actions (actual: missing). NEXT: Builder adds <button data-testid="selection-export-download">Download selected (.m3u)</button> to the library selection UI.`
	).toBeVisible();
	await expect(download, `${caseId}: download must be a native <button>.`).toHaveJSProperty(
		'tagName',
		'BUTTON'
	);
	await expect(
		download,
		`${caseId}: download accessible name must be exactly the locked label.`
	).toHaveAccessibleName(/^Download selected \(\.m3u\)$/);
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
	const saved = testInfo.outputPath(`selected-${index}.m3u`);
	await mkdir(dirname(saved), { recursive: true });
	await file.saveAs(saved);
	return { filename: file.suggestedFilename(), body: await readFile(saved, 'utf-8') };
}

async function expectStatusPoliteInMain(page: Page, caseId: string): Promise<Locator> {
	const status = statusRegion(page);
	await expect(
		status,
		`${caseId}: the export status region must live inside <main> (announcements scoped to main).`
	).toHaveCount(1);
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
async function expectActionUnavailable(
	button: Locator,
	caseId: string,
	label: string
): Promise<void> {
	if ((await button.count()) === 0) return;
	if (await button.isVisible()) {
		await expect(
			button,
			`${caseId}: ${label} must be truthfully disabled while the selection is empty (or absent/hidden). NEXT: Builder hides or disables both export actions for an empty selection.`
		).toBeDisabled();
	}
	await button.evaluate((node: HTMLButtonElement) => node.click());
}

async function expectNoFallbackShown(page: Page): Promise<void> {
	const fallback = page.locator('main').getByTestId('selection-export-fallback');
	if ((await fallback.count()) > 0) {
		await expect(fallback, 'no fallback may be shown on this path').toBeHidden();
	}
}

test.beforeEach(async ({ page, context }) => {
	const audit = { errors: [] as string[], mutations: [] as string[], downloads: [] as Download[] };
	audits.set(page, audit);
	page.on('pageerror', (error) => audit.errors.push(error.message));
	page.on('download', (download) => audit.downloads.push(download));
	page.on('request', (request) => {
		if (request.method() !== 'GET' && request.method() !== 'HEAD') {
			audit.mutations.push(`${request.method()} ${new URL(request.url()).pathname}`);
		}
	});
	await context.route('**/*', (route) => {
		const url = new URL(route.request().url());
		if (!['localhost', '127.0.0.1'].includes(url.hostname)) return route.abort('blockedbyclient');
		if (url.pathname.startsWith('/api/')) {
			return route.fulfill({ status: 404, contentType: 'application/json', body: '{}' });
		}
		return route.continue();
	});
});

test.afterEach(async ({ page }) => {
	const audit = audits.get(page)!;
	expect.soft(audit.errors, 'selection export must not cause an uncaught browser error').toEqual([]);
	expect.soft(audit.mutations, 'selection export must remain device-local').toEqual([]);
});

test('[SELECTEXPORT-01] export actions are labeled native buttons, keyboard-operable, at least 24x24 on a narrow screen', async ({
	page
}, testInfo) => {
	await page.setViewportSize({ width: 360, height: 780 });
	await open(page);
	await selectTracks(page, ACTIVATED);
	const { copy, download } = await expectExportButtons(page, 'SELECTEXPORT-01');

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
		if (testid === 'selection-export-copy' || testid === 'selection-export-download') {
			found.add(testid);
		}
	}
	expect([...found].sort(), 'SELECTEXPORT-01: both export actions must be reachable by Tab').toEqual([
		'selection-export-copy',
		'selection-export-download'
	]);

	await copy.focus();
	await page.keyboard.press('Enter');
	await expect
		.poll(() => clipboardCalls(page), 'Enter on the copy button must write the clipboard')
		.toEqual([ACTIVATION_TEXT]);

	await download.focus();
	await page.keyboard.press('Space');
	const { filename, body } = await downloadBody(page, 0, testInfo);
	expect(filename, 'keyboard-activated download must be named selected.m3u').toBe('selected.m3u');
	expect(body, 'keyboard-activated download must carry the selection M3U').toBe(ACTIVATION_M3U);
});

test('[SELECTEXPORT-02] copy writes activation order, not loaded, display, ordering, or queue order', async ({
	page
}) => {
	await open(page);
	await selectTracks(page, ACTIVATED);
	const { copy } = await expectExportButtons(page, 'SELECTEXPORT-02');

	// Diverge every other order a wrong implementation might reuse: queue
	// built Alpha then Zulu (a/z), Selected-only display with title
	// ordering (d/m/z), loaded order of the subset (z/m/d).
	await row(page, 'sel-a').click();
	await row(page, 'sel-z').click();
	await expect.poll(() => queueOrder(page)).toEqual(['sel-a', 'sel-z']);
	await orderControl(page).selectOption('title');
	await chooseShow(page, 'Selected only');
	await expectDisplayed(page, ['sel-d', 'sel-m', 'sel-z']);

	// Only explicit activation touches the clipboard.
	expect(await clipboardCalls(page), 'no clipboard write before activation').toEqual([]);

	await copy.click();
	await expect
		.poll(() => clipboardCalls(page), 'copy must write exactly the activation-ordered text')
		.toEqual([ACTIVATION_TEXT]);
	const copied = (await clipboardCalls(page))[0] ?? '';
	for (const leaked of LEAK_TOKENS) {
		expect(copied, `copied text must not contain "${leaked}"`).not.toContain(leaked);
	}

	const status = await expectStatusPoliteInMain(page, 'SELECTEXPORT-02');
	await expect(status, 'copy success must be announced').toContainText(/copied/i);
	await expect(status, 'copy success must not claim a saved file').not.toContainText(SAVED_CLAIM);
	await expect(status, 'copy success must not claim a download').not.toContainText(/download/i);
	await expectNoFallbackShown(page);
	expect(downloadsOf(page), 'copy must not start a download').toEqual([]);
});

test('[SELECTEXPORT-03] selected tracks hidden by the find field are still exported in activation order', async ({
	page
}) => {
	await open(page);
	await selectTracks(page, ACTIVATED);
	const { copy } = await expectExportButtons(page, 'SELECTEXPORT-03');

	// "harbor" keeps only the two Harbor Lights rows visible; the selected
	// Delta (Echo Band) is hidden, disclosed, and still selected.
	await findField(page).fill('harbor');
	await expectDisplayed(page, ['sel-z', 'sel-m']);
	await expect(selectionCount(page)).toContainText('3 selected');
	const hidden = page.getByTestId('selection-hidden-count');
	await expect(hidden, 'find must disclose the selected row it hides').toBeVisible();
	await expect(hidden).toContainText('1');

	await copy.click();
	await expect
		.poll(() => clipboardCalls(page), 'export covers every selected loaded track, visible or not')
		.toEqual([ACTIVATION_TEXT]);

	const status = await expectStatusPoliteInMain(page, 'SELECTEXPORT-03');
	await expect(status).toContainText(/copied/i);
	await expect(selectionCount(page)).toContainText('3 selected');
	await expect(findField(page)).toHaveValue('harbor');
});

test('[SELECTEXPORT-04] download saves selected.m3u in activation order, reads live selection on repeat, retains at most one Blob URL', async ({
	page
}, testInfo) => {
	await open(page);
	await selectTracks(page, ACTIVATED);
	const { download } = await expectExportButtons(page, 'SELECTEXPORT-04');
	const baseline = await blobCounts(page);

	await download.click();
	const first = await downloadBody(page, 0, testInfo);
	expect(first.filename, 'download must be named selected.m3u').toBe('selected.m3u');
	expect(first.body, 'downloaded bytes must be the activation-ordered selection M3U').toBe(
		ACTIVATION_M3U
	);
	for (const leaked of LEAK_TOKENS) {
		expect(first.body, `M3U bytes must not contain "${leaked}"`).not.toContain(leaked);
	}
	expect(first.body, 'M3U bytes carry no control characters beyond line breaks').not.toMatch(
		/[\0-\x09\x0B-\x1F\x7F]/
	);

	const status = await expectStatusPoliteInMain(page, 'SELECTEXPORT-04');
	await expect(status, 'download initiation must be announced').toContainText(/download/i);
	await expect(status, 'download must not claim the file was saved').not.toContainText(SAVED_CLAIM);
	await expect(status, 'download must not claim a copy happened').not.toContainText(/copied/i);
	expect(await clipboardCalls(page), 'download must not write the clipboard').toEqual([]);
	await expect.poll(async () => (await blobCounts(page)).live).toBeLessThanOrEqual(baseline.live + 1);

	// Uncheck + re-check Zulu: it moves to the END of the activation order,
	// and the second download must read that live selection.
	await checkbox(page, 'sel-z').uncheck();
	await expect(selectionCount(page)).toContainText('2 selected');
	await checkbox(page, 'sel-z').check();
	await expect(selectionCount(page)).toContainText('3 selected');
	await download.click();
	const second = await downloadBody(page, 1, testInfo);
	expect(second.filename, 'second download must be named selected.m3u').toBe('selected.m3u');
	expect(second.body, 'second download must read the live activation order').toBe(REACTIVATED_M3U);
	const after = await blobCounts(page);
	expect(
		after.created,
		'no more than one Blob URL may be created per download'
	).toBeLessThanOrEqual(baseline.created + 2);
	expect(after.live, 'at most one live Blob URL is retained').toBeLessThanOrEqual(baseline.live + 1);
	expect(await execCommandCallCount(page), 'download must not use execCommand').toBe(0);
});

test('[SELECTEXPORT-05] exporting mutates neither selection, favorites, queue, playback, view, nor storage', async ({
	page
}) => {
	await open(page);

	await row(page, 'sel-a').click();
	await row(page, 'sel-z').click();
	await expect.poll(() => queueOrder(page)).toEqual(['sel-a', 'sel-z']);
	await row(page, 'sel-d').getByRole('button', { name: 'Star Delta Song', exact: true }).click();
	await expect.poll(() => favoriteIds(page)).toEqual(['sel-d']);
	await selectTracks(page, ACTIVATED);
	const { copy, download } = await expectExportButtons(page, 'SELECTEXPORT-05');

	const before = {
		displayed: await displayedIds(page),
		queueIds: await queueOrder(page),
		nowPlaying: await page.getByTestId('now-playing').textContent(),
		selectionText: await selectionCount(page).textContent(),
		selected: await selectedStates(page, ALL_IDS),
		favorites: await favoriteIds(page),
		show: await showControl(page).locator('option:checked').textContent(),
		order: await orderControl(page).inputValue(),
		find: await findField(page).inputValue(),
		audioCount: await page.getByTestId('audio-player').count(),
		media: await mediaCallCounts(page),
		storage: await storageSnapshot(page)
	};
	expect(before.selected, 'fixture selection is d/z/m checked over loaded z/a/m/b/d').toEqual([
		true,
		false,
		true,
		false,
		true
	]);

	await copy.click();
	await expect.poll(() => clipboardCalls(page)).toEqual([ACTIVATION_TEXT]);
	await download.click();
	await expect.poll(() => downloadsOf(page).length).toBe(1);

	expect(await displayedIds(page), 'displayed order must not change').toEqual(before.displayed);
	expect(await queueOrder(page), 'queue order must not change').toEqual(before.queueIds);
	expect(await page.getByTestId('now-playing').textContent(), 'current track must not change').toBe(
		before.nowPlaying
	);
	expect(await selectionCount(page).textContent(), 'selection count must not change').toBe(
		before.selectionText
	);
	expect(await selectedStates(page, ALL_IDS), 'checkbox selection must not change').toEqual(
		before.selected
	);
	expect(await favoriteIds(page), 'favorites must not change').toEqual(before.favorites);
	expect(
		await showControl(page).locator('option:checked').textContent(),
		'view must not change'
	).toBe(before.show);
	expect(await orderControl(page).inputValue(), 'ordering must not change').toBe(before.order);
	expect(await findField(page).inputValue(), 'find query must not change').toBe(before.find);
	expect(await page.getByTestId('audio-player').count(), 'playback element must not appear/vanish').toBe(
		before.audioCount
	);
	expect(await mediaCallCounts(page), 'export must not play or pause').toEqual(before.media);
	expect(await storageSnapshot(page), 'export must not persist anything').toEqual(before.storage);

	// The selection's activation order also survives: appending still
	// queues d/z/m after the existing a/z queue (z dedupes as queued).
	await page.getByRole('button', { name: 'Append selected', exact: true }).click();
	await expect.poll(() => queueOrder(page)).toEqual(['sel-a', 'sel-z', 'sel-d', 'sel-m']);
});

test('[SELECTEXPORT-06] an empty selection offers no enabled export and claims no success', async ({
	page
}) => {
	await open(page);

	for (const testid of ['selection-export-copy', 'selection-export-download'] as const) {
		await expectActionUnavailable(
			page.locator('main').getByTestId(testid),
			'SELECTEXPORT-06',
			testid
		);
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
	// track is selected (this is what fails before the feature exists),
	// and withdraw or disable them again once the selection clears.
	await selectTracks(page, ['sel-m']);
	const { copy, download } = await expectExportButtons(page, 'SELECTEXPORT-06');
	await expect(copy, 'copy must enable once a track is selected').toBeEnabled();
	await expect(download, 'download must enable once a track is selected').toBeEnabled();

	await page.getByRole('button', { name: 'Clear selection', exact: true }).click();
	await expect(selectionCount(page)).toContainText('0 selected');
	await expectActionUnavailable(copyButton(page), 'SELECTEXPORT-06', 'copy');
	await expectActionUnavailable(downloadButton(page), 'SELECTEXPORT-06', 'download');
	expect(await clipboardCalls(page), 'still no clipboard write after clearing').toEqual([]);
	expect(downloadsOf(page), 'still no download after clearing').toEqual([]);
	if ((await status.count()) > 0) {
		await expect(status, 'clearing must not leave a success claim').not.toContainText(FALSE_SUCCESS);
	}
});

test('[SELECTEXPORT-07] an export-unsafe selected id refuses both actions truthfully, then recovers once deselected', async ({
	page
}) => {
	const items: LibraryRow[] = [
		{ id: 'sel-z', title: 'Zulu Song', artist: 'Harbor Lights' },
		{ id: 'bad/id', title: 'Bad Slash', artist: 'Guard' }
	];
	await open(page, { items });
	await selectTracks(page, ['sel-z', 'bad/id']);
	const { copy, download } = await expectExportButtons(page, 'SELECTEXPORT-07');
	const baseline = await blobCounts(page);

	await download.click();
	const status = await expectStatusPoliteInMain(page, 'SELECTEXPORT-07');
	await expect(
		status,
		'unsafe id must surface a truthful in-page error for the download'
	).toContainText(EXPORT_ERROR);
	await expect(status, 'refused download must not claim success').not.toContainText(FALSE_SUCCESS);
	expect(downloadsOf(page), 'refused download must not deliver a partial file').toEqual([]);
	expect(
		(await blobCounts(page)).created,
		'refused download must not even mint a partial-file Blob URL'
	).toBe(baseline.created);
	await expectNoFallbackShown(page);

	await copy.click();
	await expect(status, 'unsafe id must surface a truthful in-page error for the copy').toContainText(
		EXPORT_ERROR
	);
	await expect(status, 'refused copy must not claim success').not.toContainText(FALSE_SUCCESS);
	expect(await clipboardCalls(page), 'refused copy must not write the clipboard').toEqual([]);
	await expectNoFallbackShown(page);

	// The refusal changed nothing and the page is still fully usable.
	await expect(selectionCount(page)).toContainText('2 selected');
	expect(await selectedStates(page, ['sel-z', 'bad/id'])).toEqual([true, true]);
	expect(await page.getByTestId('library-item').count()).toBe(2);

	await checkbox(page, 'bad/id').uncheck();
	await expect(selectionCount(page)).toContainText('1 selected');
	await copy.click();
	await expect
		.poll(() => clipboardCalls(page), 'export must work again once the unsafe track is deselected')
		.toEqual(['1. Zulu Song — Harbor Lights']);
	await expect(status, 'recovered copy may now truthfully announce success').toContainText(/copied/i);
});

async function expectFallback(page: Page, caseId: string, expectedText: string): Promise<void> {
	const fallback = page.locator('main').getByTestId('selection-export-fallback');
	await expect(
		fallback,
		`${caseId}: expected selectable fallback text when the clipboard cannot be used (actual: missing). NEXT: Builder shows a labeled readonly selection-export-fallback holding the exact numbered text.`
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
	expect(
		await fallback.inputValue(),
		`${caseId}: fallback must hold the exact activation-ordered text`
	).toBe(expectedText);

	const instructions = page.locator('main').getByTestId('selection-export-instructions');
	await expect(
		instructions,
		`${caseId}: fallback must come with manual-copy instructions`
	).toBeVisible();
	await expect(instructions).toContainText(/copy/i);
	await expect(
		instructions,
		`${caseId}: instructions must name an actionable gesture`
	).toContainText(/ctrl|cmd|⌘|right-click|long-press|keyboard|select/i);
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

test('[SELECTEXPORT-08] a rejected clipboard write leaves selectable activation-ordered fallback text and no false success', async ({
	page
}) => {
	await open(page, { clipboard: 'rejected' });
	await selectTracks(page, ACTIVATED);
	const { copy } = await expectExportButtons(page, 'SELECTEXPORT-08');

	await copy.click();
	await expectFallback(page, 'SELECTEXPORT-08', ACTIVATION_TEXT);
	await expect(copy, 'copy stays enabled after a rejected write').toBeEnabled();
	expect(downloadsOf(page), 'a refused copy must not fall back to a download').toEqual([]);
	expect(await selectedStates(page, ALL_IDS), 'selection survives the refused copy').toEqual([
		true,
		false,
		true,
		false,
		true
	]);
});

test('[SELECTEXPORT-09] a missing Clipboard API leaves selectable activation-ordered fallback text and no false success', async ({
	page
}) => {
	await open(page, { clipboard: 'missing' });
	await selectTracks(page, ACTIVATED);
	const { copy } = await expectExportButtons(page, 'SELECTEXPORT-09');

	await copy.click();
	await expectFallback(page, 'SELECTEXPORT-09', ACTIVATION_TEXT);
	expect(await clipboardCalls(page), 'no clipboard API means no recorded write').toEqual([]);
});

test('[SELECTEXPORT-10] labels equal the shared formatting on blank/control/unicode fields and unselected rows are skipped', async ({
	page
}, testInfo) => {
	const items: LibraryRow[] = [
		{ id: 'lbl-unicode', title: 'Björk 🦢', artist: 'Sigur Rós 日本語' },
		{ id: 'lbl-skip', title: 'Skipped Song', artist: 'Never Picked' },
		{ id: 'lbl-solo', title: 'Solo Flight', artist: '   ' },
		{ id: 'lbl-blank-title', title: '   ', artist: 'Ghost Choir' },
		{ id: 'lbl-ctrl', title: 'One\nTwo\tThree', artist: 'A\0B' }
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

	await open(page, { items });
	// Activation order runs against load order; lbl-skip stays unselected.
	await selectTracks(page, ['lbl-ctrl', 'lbl-blank-title', 'lbl-solo', 'lbl-unicode']);
	const { copy, download } = await expectExportButtons(page, 'SELECTEXPORT-10');

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
	expect(filename).toBe('selected.m3u');
	expect(body, 'M3U must equal the shared label formatting in activation order').toBe(EDGE_M3U);
	expect(body, 'unselected loaded rows stay out of the export').not.toContain('lbl-skip');
	expect(body).not.toContain('Skipped Song');
});
