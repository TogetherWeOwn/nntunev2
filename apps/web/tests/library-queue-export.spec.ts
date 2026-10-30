import { expect, test, type Download, type Locator, type Page } from '@playwright/test';
import { createQueue, formatQueueM3U, formatQueueText } from '../src/lib/player/queue.js';

/**
 * Protected acceptance bank: export the authenticated /library queue, not
 * /demo/player. The paired implementation must not edit this file.
 *
 * Breaks caught: exporting library/display order instead of arranged queue
 * order; filtering exports by current/checked/visible rows; stale cached
 * exports; clipboard writes or downloads without activation; false success
 * on missing/rejected/pending clipboard; late completion after queue edits
 * (including an away-and-back edit); export-induced queue/media changes;
 * unreachable or undersized controls; assertive export announcements.
 *
 * Page contract (all hooks scoped to main): native buttons named exactly
 * "Copy queue as text" and "Download queue (.m3u)"; queue-export-status
 * (polite), queue-export-fallback (labeled readonly input/textarea), and
 * queue-export-instructions (manual-copy gesture). Empty queues may hide
 * or disable actions, but must explain why. A download announces initiation,
 * never claims that the browser saved the file. Reuse the shared queue.ts
 * formatters; their exact format is owned by queueExport.acceptance.test.ts.
 *
 * Run from repo root, with preview port 4173 free:
 *   CI=1 pnpm --dir apps/web exec playwright test tests/library-queue-export.spec.ts --workers=1
 * All APIs and media are synthetic; no production/staging access or login.
 * A successful /api/library response models the authenticated library session,
 * following the neighbouring library route tests. No demo route is visited.
 */

const ITEMS = [
	{ id: 'export-zulu', title: 'Zulu Song', artist: 'Queue Ensemble' },
	{ id: 'export-alpha', title: 'Alpha 🌊' },
	{ id: 'export-mike', title: 'Míke Tune', artist: 'Session Players' }
].map((item) => ({ ...item, streamUrl: `/test-audio/${item.id}?grant=SYNTHETIC_EXPORT_GRANT` }));
const INITIAL_IDS = ['export-mike', 'export-zulu', 'export-alpha'];
const ARRANGED_IDS = ['export-mike', 'export-alpha', 'export-zulu'];
const ARRANGED_TEXT = '1. Míke Tune — Session Players\n2. Alpha 🌊\n3. Zulu Song — Queue Ensemble';
const ARRANGED_M3U =
	'#EXTM3U\n#EXTINF:-1,Míke Tune — Session Players\nexport-mike\n' +
	'#EXTINF:-1,Alpha 🌊\nexport-alpha\n#EXTINF:-1,Zulu Song — Queue Ensemble\nexport-zulu';
const COPY_NAME = 'Copy queue as text';
const DOWNLOAD_NAME = 'Download queue (.m3u)';
const SUCCESS = /\bcopied\b|\bsuccess(?:ful(?:ly)?)?\b/i;

type ClipboardMode = 'fulfilled' | 'missing' | 'rejected' | 'delayed';
type PendingWrite = { resolve: () => void; reject: (reason: unknown) => void };
type Probe = {
	writes: string[];
	pending: PendingWrite[];
	media: { play: number; pause: number };
};
type ProbedWindow = Window & { __libraryQueueExport: Probe };

function main(page: Page) {
	return page.getByRole('main');
}
function status(page: Page) {
	return main(page).getByTestId('queue-export-status');
}
function fallback(page: Page) {
	return main(page).getByTestId('queue-export-fallback');
}
function queueRow(page: Page, id: string) {
	return main(page).locator(`[data-testid="queue-item"][data-track-id="${id}"]`);
}
function moveAlpha(page: Page, direction: 'up' | 'down') {
	return queueRow(page, 'export-alpha').getByRole('button', {
		name: `Move Alpha 🌊 ${direction}`, exact: true
	});
}
function clearQueue(page: Page) {
	return main(page).getByRole('button', { name: 'Clear queue', exact: true });
}
async function order(page: Page) {
	return main(page).getByTestId('queue-item').evaluateAll((rows) =>
		rows.map((row) => row.getAttribute('data-track-id'))
	);
}
async function writes(page: Page) {
	return page.evaluate(() => (window as unknown as ProbedWindow).__libraryQueueExport.writes);
}
async function addTrack(page: Page, id: string) {
	const item = ITEMS.find((item) => item.id === id)!;
	const title = item.title.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
	await main(page).locator(`[data-testid="library-item"][data-id="${id}"]`)
		.getByRole('button', { name: new RegExp(`^${title}( — |$)`) }).click();
}

/** Replace only external boundaries: library fetch, media I/O and OS clipboard. */
async function openLibrary(page: Page, clipboard: ClipboardMode = 'fulfilled', seed = true) {
	const downloads: string[] = [];
	page.on('download', (file) => downloads.push(file.suggestedFilename()));
	await page.route('**/api/library', (route) => route.fulfill({ json: ITEMS }));
	await page.route('**/test-audio/**', () => {});
	await page.addInitScript((mode: ClipboardMode) => {
		const probe: Probe = { writes: [], pending: [], media: { play: 0, pause: 0 } };
		(window as unknown as ProbedWindow).__libraryQueueExport = probe;
		if (mode === 'missing') {
			Object.defineProperty(navigator, 'clipboard', { configurable: true, value: undefined });
		} else {
			Object.defineProperty(navigator, 'clipboard', {
				configurable: true,
				value: { writeText: (text: string) => {
					probe.writes.push(text);
					if (mode === 'rejected') return Promise.reject(new DOMException('Denied', 'NotAllowedError'));
					if (mode === 'delayed') return new Promise<void>((resolve, reject) => {
						probe.pending.push({ resolve, reject });
					});
					return Promise.resolve();
				} }
			});
		}
		HTMLMediaElement.prototype.play = function () {
			probe.media.play += 1;
			return Promise.resolve();
		};
		HTMLMediaElement.prototype.pause = function () { probe.media.pause += 1; };
	}, clipboard);
	await page.goto('/library');
	await expect(page).toHaveURL(/\/library$/);
	await expect(main(page).getByTestId('library-item')).toHaveCount(ITEMS.length);
	if (seed) {
		for (const id of INITIAL_IDS) await addTrack(page, id);
		await expect(main(page).getByTestId('queue-item')).toHaveCount(3);
		expect(await order(page)).toEqual(INITIAL_IDS);
		await expect(queueRow(page, 'export-mike')).toHaveAttribute('aria-current', 'true');
		await expect(main(page).getByTestId('audio-player')).toHaveCount(1);
	}
	return downloads;
}

async function actions(page: Page) {
	const copy = main(page).getByRole('button', { name: COPY_NAME, exact: true });
	const download = main(page).getByRole('button', { name: DOWNLOAD_NAME, exact: true });
	await expect(copy, 'library export missing: add Copy queue as text to the non-empty library queue').toBeVisible();
	await expect(download, 'library export missing: add Download queue (.m3u) to the non-empty library queue').toBeVisible();
	for (const button of [copy, download]) {
		await expect(button).toHaveJSProperty('tagName', 'BUTTON');
		await expect(button).toBeEnabled();
	}
	return { copy, download };
}

function exportState(ids: string[]) {
	return createQueue(ids.map((id) => {
		const item = ITEMS.find((item) => item.id === id)!;
		return { ...item, artist: item.artist ?? '' };
	}));
}

async function arrange(page: Page) {
	await moveAlpha(page, 'up').click();
	expect(await order(page)).toEqual(ARRANGED_IDS);
	// Select a non-first queue row: export must still include every track.
	await queueRow(page, 'export-zulu').getByRole('button', { name: 'Select Zulu Song', exact: true }).click();
	await expect(queueRow(page, 'export-zulu')).toHaveAttribute('aria-current', 'true');
}

async function readDownload(file: Download) {
	expect(file.suggestedFilename()).toMatch(/\.m3u$/i);
	expect(await file.failure()).toBeNull();
	const stream = await file.createReadStream();
	expect(stream).not.toBeNull();
	stream!.setEncoding('utf8');
	let body = '';
	for await (const chunk of stream!) body += chunk;
	return body;
}

async function downloadedBody(page: Page, button: Locator) {
	const pending = page.waitForEvent('download');
	await button.click();
	return readDownload(await pending);
}

async function expectPolite(page: Page) {
	await expect(status(page)).toBeAttached();
	const attributes = await status(page).evaluate((element) => ({
		live: element.getAttribute('aria-live'), role: element.getAttribute('role'),
		assertive: Boolean(element.closest('[aria-live="assertive"], [role="alert"]'))
	}));
	expect(attributes.live === 'polite' || (attributes.role === 'status' && attributes.live !== 'off')).toBe(true);
	expect(attributes.assertive).toBe(false);
	// Svelte's page-wide announcer is outside main and is not an export region.
	await expect(main(page).locator('[aria-live="assertive"], [role="alert"]')).toHaveCount(0);
}

/** Snapshot real observable state, not the formatters or a fake queue. */
async function snapshot(page: Page) {
	return main(page).evaluate((element) => {
		const audio = element.querySelector<HTMLAudioElement>('[data-testid="audio-player"]');
		const dump = (storage: Storage) => Object.fromEntries(
			Array.from({ length: storage.length }, (_, i) => storage.key(i)!)
				.sort().map((key) => [key, storage.getItem(key)])
		);
		return {
			queue: Array.from(element.querySelectorAll('[data-testid="queue-item"]')).map((row) => ({
				id: row.getAttribute('data-track-id'), current: row.getAttribute('aria-current')
			})),
			checked: Array.from(element.querySelectorAll<HTMLInputElement>('[data-testid="library-item"] input[type="checkbox"]')).map((input) => input.checked),
			nowPlaying: element.querySelector('[data-testid="now-playing"]')?.textContent,
			media: { ...(window as unknown as ProbedWindow).__libraryQueueExport.media },
			audio: audio && { src: audio.getAttribute('src'), time: audio.currentTime, rate: audio.playbackRate, paused: audio.paused },
			local: dump(localStorage), session: dump(sessionStorage)
		};
	});
}

async function settle(page: Page, outcome: 'resolve' | 'reject') {
	await expect.poll(() => page.evaluate(
		() => (window as unknown as ProbedWindow).__libraryQueueExport.pending.length
	)).toBeGreaterThan(0);
	await page.evaluate(async (result) => {
		const pending = (window as unknown as ProbedWindow).__libraryQueueExport.pending.shift();
		if (!pending) throw new Error('Expected a pending queue clipboard write');
		if (result === 'resolve') pending.resolve();
		else pending.reject(new DOMException('Denied', 'NotAllowedError'));
		// Let the real clipboard continuation and Svelte's DOM flush settle.
		await new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
	}, outcome);
}

async function expectNoSuccess(page: Page) {
	// Region may be absent after emptying the queue; absence is valid here.
	const messages = await status(page).allTextContents();
	expect(messages.join(' ')).not.toMatch(SUCCESS);
}

test('harness control: library arrangement preserves selection and shared format vocabulary', async ({ page }) => {
	const downloads = await openLibrary(page);
	await arrange(page);
	expect(formatQueueText(exportState(ARRANGED_IDS))).toBe(ARRANGED_TEXT);
	expect(formatQueueM3U(exportState(ARRANGED_IDS))).toBe(ARRANGED_M3U);
	const before = await snapshot(page);
	expect(before.queue).toEqual([
		{ id: 'export-mike', current: null },
		{ id: 'export-alpha', current: null },
		{ id: 'export-zulu', current: 'true' }
	]);
	expect(before.audio?.src).toBe('/test-audio/export-zulu?grant=SYNTHETIC_EXPORT_GRANT');
	await queueRow(page, 'export-alpha').getByRole('button', {
		name: 'Remove Alpha 🌊 from queue', exact: true
	}).click();
	expect(await order(page)).toEqual(['export-mike', 'export-zulu']);
	await addTrack(page, 'export-alpha');
	expect(await order(page)).toEqual(INITIAL_IDS);
	await expect(queueRow(page, 'export-zulu')).toHaveAttribute('aria-current', 'true');
	expect(await writes(page)).toEqual([]);
	expect(downloads).toEqual([]);
});

test('copy exports exactly the shared text in arranged order, not library, checked or filtered order', async ({ page }) => {
	const downloads = await openLibrary(page);
	await arrange(page);
	await main(page).getByRole('checkbox', { name: 'Select Alpha 🌊', exact: true }).check();
	await main(page).getByLabel('Filter queue', { exact: true }).fill('Alpha');
	await expect(main(page).getByTestId('queue-item')).toHaveCount(1);
	const { copy } = await actions(page);
	expect(await writes(page), 'no write from load, arrangement, selection or filtering').toEqual([]);
	expect(downloads).toEqual([]);
	// Hand-checked literal also guards the shared vocabulary (no new format).
	expect(formatQueueText(exportState(ARRANGED_IDS))).toBe(ARRANGED_TEXT);
	await copy.click();
	await expect.poll(() => writes(page)).toEqual([ARRANGED_TEXT]);
	await expect(status(page)).toContainText(/\bcopied\b/i);
	await expectPolite(page);
	await expect(copy).toBeFocused();
	expect(downloads, 'copy never initiates a download').toEqual([]);
});

test('download body is exactly shared M3U for the arranged whole queue and feedback only claims initiation', async ({ page }) => {
	await openLibrary(page);
	await arrange(page);
	await main(page).getByLabel('Filter queue', { exact: true }).fill('Alpha');
	const { download } = await actions(page);
	expect(formatQueueM3U(exportState(ARRANGED_IDS))).toBe(ARRANGED_M3U);
	expect(await downloadedBody(page, download)).toBe(ARRANGED_M3U);
	await expect(status(page)).toContainText(/\bdownload\b.*\b(started|starting|requested|initiated)\b|\b(started|starting|requested|initiated)\b.*\bdownload\b/i);
	await expect(status(page)).not.toContainText(/\b(saved|downloaded|copied|success(?:ful(?:ly)?)?)\b/i);
	await expectPolite(page);
	expect(await writes(page)).toEqual([]);
});

test('successive exports sample live queue after reorder and remove instead of a cached initial list', async ({ page }) => {
	await openLibrary(page);
	const { copy, download } = await actions(page);
	await copy.click();
	await expect.poll(() => writes(page)).toEqual([formatQueueText(exportState(INITIAL_IDS))]);
	await arrange(page);
	await expectNoSuccess(page);
	await queueRow(page, 'export-mike').getByRole('button', { name: 'Remove Míke Tune from queue', exact: true }).click();
	const remaining = ['export-alpha', 'export-zulu'];
	expect(await order(page)).toEqual(remaining);
	await copy.click();
	await expect.poll(() => writes(page)).toEqual([
		formatQueueText(exportState(INITIAL_IDS)), '1. Alpha 🌊\n2. Zulu Song — Queue Ensemble'
	]);
	expect(await downloadedBody(page, download)).toBe(formatQueueM3U(exportState(remaining)));
});

for (const emptyAt of ['initial', 'after clear'] as const) {
	test(`empty queue (${emptyAt}) hides or disables exports with a reason, without writes, downloads or success`, async ({ page }) => {
		const downloads = await openLibrary(page, 'fulfilled', emptyAt !== 'initial');
		if (emptyAt === 'after clear') {
			// Guard the new feature first: pre-feature main is RED, not a vacuous pass.
			await actions(page);
			await clearQueue(page).click();
			await main(page).getByRole('button', { name: 'Yes, clear queue', exact: true }).click();
		}
		await expect(main(page).getByTestId('queue-item')).toHaveCount(0);
		for (const name of [COPY_NAME, DOWNLOAD_NAME]) {
			const button = main(page).getByRole('button', { name, exact: true });
			if (await button.count()) {
				await expect(button).toBeDisabled();
				// Native disabled controls must also reject synthetic activation.
				await button.evaluate((element: HTMLButtonElement) => element.click());
			}
		}
		const reason = main(page).getByTestId('now-playing');
		await expect(reason).toBeVisible();
		await expect(reason).toContainText(/queue\s+is\s+empty/i);
		await expectNoSuccess(page);
		await expect(fallback(page)).toHaveCount(0);
		expect(await writes(page)).toEqual([]);
		expect(downloads).toEqual([]);
		// Re-adding demonstrates recovery and makes the initial-empty case RED on main too.
		await addTrack(page, 'export-alpha');
		const { copy } = await actions(page);
		await copy.click();
		await expect.poll(() => writes(page)).toEqual(['1. Alpha 🌊']);
	});
}

for (const mode of ['missing', 'rejected'] as const) {
	test(`${mode} clipboard shows selectable exact fallback and Clipboard unavailable, never Copied`, async ({ page }) => {
		const downloads = await openLibrary(page, mode);
		await arrange(page);
		const { copy } = await actions(page);
		const before = await snapshot(page);
		await copy.click();
		await expect(fallback(page)).toBeVisible();
		await expect(fallback(page)).toHaveAccessibleName(/\S/);
		await expect(fallback(page)).toHaveJSProperty('readOnly', true);
		expect(await fallback(page).inputValue()).toBe(ARRANGED_TEXT);
		await expect(status(page)).toContainText(/clipboard unavailable/i);
		await expectNoSuccess(page);
		const instructions = main(page).getByTestId('queue-export-instructions');
		await expect(instructions).toBeVisible();
		await expect(instructions).toContainText(/copy/i);
		await expect(instructions).toContainText(/ctrl|cmd|⌘|right.click|long.press|keyboard/i);
		await expect(instructions).not.toContainText(SUCCESS);
		await expectPolite(page);
		await expect(copy).toBeFocused();
		expect(await snapshot(page)).toEqual(before);
		await expect(fallback(page)).toBeEnabled();
		await fallback(page).focus();
		await expect(fallback(page)).toBeFocused();
		await page.keyboard.press('ControlOrMeta+A');
		expect(await fallback(page).evaluate((element: HTMLInputElement | HTMLTextAreaElement) => [element.selectionStart, element.selectionEnd])).toEqual([0, ARRANGED_TEXT.length]);
		expect(await writes(page)).toEqual(mode === 'missing' ? [] : [ARRANGED_TEXT]);
		expect(downloads).toEqual([]);
	});
}

test('copy and download leave queue order, current track, checked selection, playback and storage untouched', async ({ page }) => {
	await openLibrary(page);
	await arrange(page);
	const { copy, download } = await actions(page);
	await main(page).getByRole('checkbox', { name: 'Select Alpha 🌊', exact: true }).check();
	const audio = main(page).getByTestId('audio-player');
	await audio.evaluate((element: HTMLAudioElement) => { element.currentTime = 37.25; element.playbackRate = 1.5; });
	const before = await snapshot(page);
	const mediaNode = await audio.elementHandle();
	await copy.click();
	await expect(status(page)).toContainText(/\bcopied\b/i);
	expect(await snapshot(page)).toEqual(before);
	await downloadedBody(page, download);
	expect(await snapshot(page)).toEqual(before);
	expect(await audio.evaluate((element, original) => element === original, mediaNode)).toBe(true);
});

for (const edit of ['reorder', 'clear', 'away and back', 'remove', 'append', 'selection'] as const) {
	for (const outcome of ['resolve', 'reject'] as const) {
		test(`${edit} invalidates a pending copy before late ${outcome}, and a fresh copy recovers`, async ({ page }) => {
			await openLibrary(page, 'delayed');
			const { copy } = await actions(page);
			// Prepare a genuinely appendable row before the clipboard action starts.
			if (edit === 'append') {
				await queueRow(page, 'export-alpha').getByRole('button', {
					name: 'Remove Alpha 🌊 from queue', exact: true
				}).click();
			}
			const startIds = edit === 'append' ? ['export-mike', 'export-zulu'] : INITIAL_IDS;
			await copy.click();
			await expect.poll(() => writes(page)).toEqual([formatQueueText(exportState(startIds))]);
			await expectNoSuccess(page);
			let expectedIds: string[] = INITIAL_IDS;
			if (edit === 'clear') {
				await clearQueue(page).click();
				await main(page).getByRole('button', { name: 'Yes, clear queue', exact: true }).click();
				expectedIds = ['export-alpha']; // Added again only after the stale completion.
			} else if (edit === 'append') {
				await addTrack(page, 'export-alpha');
			} else if (edit === 'remove') {
				await queueRow(page, 'export-zulu').getByRole('button', {
					name: 'Remove Zulu Song from queue', exact: true
				}).click();
				expectedIds = ['export-mike', 'export-alpha'];
			} else if (edit === 'selection') {
				await queueRow(page, 'export-zulu').getByRole('button', {
					name: 'Select Zulu Song', exact: true
				}).click();
				await expect(queueRow(page, 'export-zulu')).toHaveAttribute('aria-current', 'true');
			} else {
				await moveAlpha(page, 'up').click();
				if (edit === 'away and back') await moveAlpha(page, 'down').click();
				else expectedIds = ARRANGED_IDS;
			}
			expect(await order(page)).toEqual(edit === 'clear' ? [] : expectedIds);
			const beforeCompletion = await snapshot(page);
			const feedback = {
				status: await status(page).allTextContents(),
				instructions: await main(page).getByTestId('queue-export-instructions').allTextContents()
			};
			await settle(page, outcome);
			await expectNoSuccess(page);
			await expect(fallback(page)).toHaveCount(0);
			expect({
				status: await status(page).allTextContents(),
				instructions: await main(page).getByTestId('queue-export-instructions').allTextContents()
			}, 'superseded completion must not publish success, failure or manual-copy feedback').toEqual(feedback);
			expect(await snapshot(page)).toEqual(beforeCompletion);
			if (edit === 'clear') await addTrack(page, 'export-alpha');
			const fresh = await actions(page);
			await fresh.copy.click();
			await expect.poll(async () => (await writes(page)).length).toBe(2);
			await settle(page, 'resolve');
			await expect(status(page)).toContainText(/\bcopied\b/i);
			expect((await writes(page))[1]).toBe(formatQueueText(exportState(expectedIds)));
		});
	}
}

test('completed success and fallback clear when the queue changes', async ({ page }) => {
	await openLibrary(page, 'delayed');
	const { copy } = await actions(page);
	await copy.click();
	await settle(page, 'resolve');
	await expect(status(page)).toContainText(/\bcopied\b/i);
	await moveAlpha(page, 'up').click();
	await expectNoSuccess(page);
	await copy.click();
	await settle(page, 'reject');
	await expect(fallback(page)).toHaveValue(ARRANGED_TEXT);
	await moveAlpha(page, 'down').click();
	await expect(fallback(page)).toHaveCount(0);
	await expectNoSuccess(page);
});

for (const width of [1280, 360]) {
	test(`both actions are Tab-reachable, keyboard-operable and at least 24x24 at ${width}px`, async ({ page }) => {
		await page.setViewportSize({ width, height: 800 });
		await openLibrary(page);
		const { copy, download } = await actions(page);
		await main(page).getByRole('heading', { level: 1 }).evaluate((element: HTMLElement) => {
			element.tabIndex = -1; element.focus();
		});
		const reached = new Set<string>();
		for (let tabs = 0; tabs < 150 && reached.size < 2; tabs += 1) {
			await page.keyboard.press('Tab');
			for (const [name, button] of [[COPY_NAME, copy], [DOWNLOAD_NAME, download]] as const) {
				if (await button.evaluate((element) => element === document.activeElement)) reached.add(name);
			}
		}
		expect([...reached].sort()).toEqual([COPY_NAME, DOWNLOAD_NAME].sort());
		for (const button of [copy, download]) {
			await button.scrollIntoViewIfNeeded();
			const box = await button.boundingBox();
			expect(box).not.toBeNull();
			expect(box!.width).toBeGreaterThanOrEqual(24);
			expect(box!.height).toBeGreaterThanOrEqual(24);
			expect(box!.x).toBeGreaterThanOrEqual(0);
			expect(box!.x + box!.width).toBeLessThanOrEqual(width);
		}
		await copy.focus();
		await page.keyboard.press('Enter');
		await expect.poll(() => writes(page)).toEqual([formatQueueText(exportState(INITIAL_IDS))]);
		await expect(status(page)).toContainText(/\bcopied\b/i);
		await download.focus();
		const pending = page.waitForEvent('download');
		await page.keyboard.press('Space');
		expect(await readDownload(await pending)).toBe(formatQueueM3U(exportState(INITIAL_IDS)));
		await expectPolite(page);
	});
}
