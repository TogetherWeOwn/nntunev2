import { expect, test, type Page } from '@playwright/test';
import { mkdir, readFile } from 'node:fs/promises';
import { dirname } from 'node:path';

/**
 * Test-Author owned RED acceptance for QUEUEEXPORT: copy or download the
 * arranged demo queue as a track list. The Builder wires `/demo/player` (plus
 * the queue.ts export formatters) to satisfy this file and must not edit it.
 *
 * LOCKED PAGE CONTRACT (new hooks on `/demo/player`; existing share-copy,
 * queue, and comfort hooks are unchanged):
 * - `queue-export-copy`: a NATIVE <button> labeled "Copy queue as text"
 *   (accessible name matches /copy queue as text/i). Keyboard reachable with
 *   visible focus (:focus-visible + outline/box-shadow). Disabled while the
 *   queue is empty.
 * - `queue-export-download`: a NATIVE <button> labeled "Download queue.m3u"
 *   (accessible name matches /download queue\.m3u/i). Keyboard reachable with
 *   visible focus. Disabled while the queue is empty.
 * - `queue-export-status`: aria-live="polite" region (or role="status"). Copy
 *   success announces a concise (<100 chars) confirmation matching /copied/i;
 *   download success matches /downloaded|saved/i. Fallback/refusal/empty
 *   never claim success (no /copied|success/i on a failed copy). Cleared on
 *   queue change (select/move/add/remove/clear) and on fresh mount; late
 *   completion from a superseded action is ignored. While the queue is empty
 *   the region names it (/empty/i) as the reason both actions are disabled.
 * - `queue-export-fallback`: a LABELED readonly <input> or <textarea> holding
 *   the track-list text as its value (exact export bytes as safe text, never
 *   innerHTML). Shown only when the Clipboard API is absent or rejects.
 *   Focusable and selectable (select() covers the full value). Focus stays on
 *   the copy button when the fallback appears (no focus steal).
 * - `queue-export-instructions`: actionable manual-copy instructions shown
 *   with the fallback (names a copy gesture: Ctrl/Cmd/right-click/etc.,
 *   matches /copy/i). Never claims success (no /copied|success/i).
 * - Activation reads the CURRENT queue order at click time (no stale
 *   snapshot) and exports the WHOLE queue regardless of selection: numbered
 *   `N. title — artist` lines via the Clipboard API, or UTF-8 M3U bytes
 *   (`#EXTM3U` + `#EXTINF` + bare id lines) as a Blob download named
 *   queue.m3u. Never stream URLs, signed grants, provider info, or identity.
 * - Only explicit activation touches the clipboard or starts a download: no
 *   writes on load, select, move, add, remove, clear, seek, or timeupdate. No
 *   `document.execCommand` calls, no backend contact, no new persistence.
 * - Copy and download never play, pause, seek, change queue/order/rate, or
 *   persist anything (localStorage and sessionStorage unchanged).
 * - Scope is the existing synthetic demo queue on `/demo/player` only.
 *
 * RED NOW: the export hooks do not exist, so every case below fails at its
 * button assertion with a NEXT action. No import/compile failure counts
 * as RED: this file imports only `@playwright/test` and node builtins.
 */

const COPY_LABEL = /copy queue as text/i;
const DOWNLOAD_LABEL = /download queue\.m3u/i;

const DEFAULT_TEXT =
	'1. Demo track — Fixture Band\n' +
	'2. Second Signal — Fixture Band\n' +
	'3. Third Light — Session Players';

const DEFAULT_M3U =
	'#EXTM3U\n' +
	'#EXTINF:-1,Demo track — Fixture Band\n' +
	'demo-track\n' +
	'#EXTINF:-1,Second Signal — Fixture Band\n' +
	'second-signal\n' +
	'#EXTINF:-1,Third Light — Session Players\n' +
	'third-light';

// After moving demo-track one slot down: second-signal, demo-track, third-light.
const MOVED_DOWN_TEXT =
	'1. Second Signal — Fixture Band\n' +
	'2. Demo track — Fixture Band\n' +
	'3. Third Light — Session Players';

const MOVED_DOWN_M3U =
	'#EXTM3U\n' +
	'#EXTINF:-1,Second Signal — Fixture Band\n' +
	'second-signal\n' +
	'#EXTINF:-1,Demo track — Fixture Band\n' +
	'demo-track\n' +
	'#EXTINF:-1,Third Light — Session Players\n' +
	'third-light';

// After clearing and re-adding third-light, demo-track, second-signal in that order.
const REadded_TEXT =
	'1. Third Light — Session Players\n' +
	'2. Demo track — Fixture Band\n' +
	'3. Second Signal — Fixture Band';

const LEAK_TOKENS = ['/api/', 'http', 'token', 'grant', 'bearer', 'provider', '@'];

type ClipboardMode = 'fulfilled' | 'rejected' | 'missing' | 'delayed';

/** Keep media synthetic and pending; stub clipboard/media per mode. */
async function openQueue(page: Page, options: { clipboard?: ClipboardMode } = {}) {
	const { clipboard = 'fulfilled' } = options;
	await page.route('**/api/demo/audio**', () => {});
	await page.addInitScript((mode: ClipboardMode) => {
		const calls: string[] = [];
		const control: {
			calls: string[];
			deferred: { resolve: () => void; reject: (error: unknown) => void } | null;
		} = { calls, deferred: null };
		(window as unknown as { __queueExportCalls: string[] }).__queueExportCalls = calls;
		(window as unknown as { __queueExportControl: unknown }).__queueExportControl = control;
		const writeText = (text: string) => {
			calls.push(text);
			if (mode === 'rejected')
				return Promise.reject(new DOMException('Denied', 'NotAllowedError'));
			if (mode === 'delayed')
				return new Promise<void>((resolve, reject) => {
					control.deferred = { resolve, reject };
				});
			return Promise.resolve();
		};
		if (mode === 'missing') {
			Object.defineProperty(navigator, 'clipboard', { value: undefined, configurable: true });
		} else {
			Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
		}
		const execCalls: unknown[][] = [];
		(window as unknown as { __execCommandCalls: unknown[][] }).__execCommandCalls = execCalls;
		(document as unknown as { execCommand: (...args: unknown[]) => boolean }).execCommand = (
			...args: unknown[]
		) => {
			execCalls.push(args);
			return false;
		};
		const mediaCalls = { play: 0, pause: 0 };
		(window as unknown as { __mediaCalls: { play: number; pause: number } }).__mediaCalls =
			mediaCalls;
		HTMLMediaElement.prototype.play = function () {
			mediaCalls.play += 1;
			return new Promise<void>(() => {});
		};
		HTMLMediaElement.prototype.pause = function () {
			mediaCalls.pause += 1;
		};
		try {
			sessionStorage.removeItem('nntune:demo-queue:v1');
			localStorage.removeItem('nntune:playback-speed:v1');
			localStorage.removeItem('nntune:resume:v1');
		} catch {
			// Storage may be blocked; the queue fixture still mounts.
		}
	}, clipboard);
	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
}

async function clipboardCalls(page: Page): Promise<string[]> {
	return page.evaluate(
		() => (window as unknown as { __queueExportCalls: string[] }).__queueExportCalls ?? []
	);
}

async function execCommandCallCount(page: Page): Promise<number> {
	return page.evaluate(
		() => ((window as unknown as { __execCommandCalls: unknown[] }).__execCommandCalls ?? []).length
	);
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

async function setCurrentTime(page: Page, seconds: number) {
	await page
		.getByTestId('demo-audio')
		.evaluate((el, value) => {
			(el as HTMLAudioElement).currentTime = value;
		}, seconds);
}

async function currentTime(page: Page) {
	return page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => el.currentTime);
}

async function queueOrder(page: Page) {
	return page
		.getByTestId('queue-item')
		.evaluateAll((els) => els.map((el) => el.getAttribute('data-track-id')));
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

async function resolveDelayedWrite(page: Page) {
	await page.evaluate(() => {
		const control = (
			window as unknown as {
				__queueExportControl: { deferred: { resolve: () => void } | null };
			}
		).__queueExportControl;
		if (!control.deferred) throw new Error('no deferred clipboard write is pending');
		control.deferred.resolve();
	});
}

async function expectExportButtons(page: Page, caseId: string) {
	const copy = page.getByTestId('queue-export-copy');
	await expect(
		copy,
		`${caseId}: expected a labeled native Copy button (actual: missing). NEXT: Builder adds <button data-testid="queue-export-copy">Copy queue as text</button> to /demo/player.`
	).toBeVisible();
	await expect(copy, `${caseId}: expected a native <button>.`).toHaveJSProperty(
		'tagName',
		'BUTTON'
	);
	await expect(copy, `${caseId}: expected accessible name "Copy queue as text".`).toHaveAccessibleName(
		COPY_LABEL
	);
	const download = page.getByTestId('queue-export-download');
	await expect(
		download,
		`${caseId}: expected a labeled native Download button (actual: missing). NEXT: Builder adds <button data-testid="queue-export-download">Download queue.m3u</button> to /demo/player.`
	).toBeVisible();
	await expect(download, `${caseId}: expected a native <button>.`).toHaveJSProperty(
		'tagName',
		'BUTTON'
	);
	await expect(
		download,
		`${caseId}: expected accessible name "Download queue.m3u".`
	).toHaveAccessibleName(DOWNLOAD_LABEL);
	return { copy, download };
}

test('[QUEUEEXPORT-01] export actions are labeled native buttons, keyboard-focusable with visible focus', async ({
	page
}) => {
	await openQueue(page);
	const { copy } = await expectExportButtons(page, 'QUEUEEXPORT-01');

	// Keyboard reachability + visible focus for both actions.
	const found = new Set<string>();
	for (let tab = 0; tab < 80 && found.size < 2; tab += 1) {
		await page.keyboard.press('Tab');
		const focused = await page.evaluate(() => {
			const el = document.activeElement as HTMLElement | null;
			if (!el) return null;
			const style = window.getComputedStyle(el);
			return {
				testid: el.getAttribute('data-testid'),
				focusVisible: el.matches(':focus-visible'),
				outlineWidth: style.outlineWidth,
				boxShadow: style.boxShadow
			};
		});
		if (
			(focused?.testid === 'queue-export-copy' || focused?.testid === 'queue-export-download') &&
			!found.has(focused.testid)
		) {
			found.add(focused.testid);
			expect(focused.focusVisible, `${focused.testid} must match :focus-visible`).toBe(true);
			const visible =
				focused.outlineWidth !== '0px' && focused.outlineWidth !== '0'
					? true
					: focused.boxShadow !== 'none';
			expect(visible, `${focused.testid} must show a visible focus indicator`).toBe(true);
		}
	}
	expect(
		[...found].sort(),
		'QUEUEEXPORT-01: both export actions must be reachable by Tab'
	).toEqual(['queue-export-copy', 'queue-export-download']);

	// Keyboard operation: Enter activates the copy.
	await copy.focus();
	await page.keyboard.press('Enter');
	await expect
		.poll(() => clipboardCalls(page), 'Enter on queue-export-copy must write the clipboard')
		.toHaveLength(1);
	const [written] = await clipboardCalls(page);
	expect(written, 'keyboard-activated copy must export the arranged queue').toBe(DEFAULT_TEXT);
});

test('[QUEUEEXPORT-02] copy writes the numbered track list in queue order', async ({ page }) => {
	await openQueue(page);
	const { copy } = await expectExportButtons(page, 'QUEUEEXPORT-02');

	expect(await clipboardCalls(page), 'no clipboard write before activation').toEqual([]);

	await copy.click();
	await expect.poll(() => clipboardCalls(page)).toHaveLength(1);
	const [written] = await clipboardCalls(page);
	expect(written, 'copied text must be the numbered track list').toBe(DEFAULT_TEXT);

	await expect(page.getByTestId('queue-export-status')).toContainText(/copied/i);
});

test('[QUEUEEXPORT-03] copy follows the arranged order after moving tracks', async ({ page }) => {
	await openQueue(page);
	const { copy } = await expectExportButtons(page, 'QUEUEEXPORT-03');

	await page.getByTestId('queue-move-down-demo-track').click();
	expect(await queueOrder(page)).toEqual(['second-signal', 'demo-track', 'third-light']);

	await copy.click();
	await expect.poll(() => clipboardCalls(page)).toHaveLength(1);
	const [written] = await clipboardCalls(page);
	expect(written, 'copied text must follow the arranged order').toBe(MOVED_DOWN_TEXT);
});

test('[QUEUEEXPORT-04] successive exports read live state: reorder and clear/re-add are never stale', async ({
	page
}) => {
	await openQueue(page);
	const { copy } = await expectExportButtons(page, 'QUEUEEXPORT-04');

	await copy.click();
	await expect.poll(() => clipboardCalls(page)).toHaveLength(1);

	await page.getByTestId('queue-move-down-demo-track').click();
	await copy.click();
	await expect.poll(() => clipboardCalls(page)).toHaveLength(2);

	await page.getByTestId('queue-clear').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await page.getByTestId('queue-add-third-light').click();
	await page.getByTestId('queue-add-demo-track').click();
	await page.getByTestId('queue-add-second-signal').click();
	expect(await queueOrder(page)).toEqual(['third-light', 'demo-track', 'second-signal']);
	await copy.click();
	await expect.poll(() => clipboardCalls(page)).toHaveLength(3);

	const calls = await clipboardCalls(page);
	expect(calls[0], 'first copy must export the default order').toBe(DEFAULT_TEXT);
	expect(calls[1], 'second copy must export the rearranged order').toBe(MOVED_DOWN_TEXT);
	expect(calls[2], 'third copy must export the re-added order').toBe(READDED_TEXT);
});

test('[QUEUEEXPORT-05] selection never filters the export: the whole queue is copied', async ({
	page
}) => {
	await openQueue(page);
	const { copy } = await expectExportButtons(page, 'QUEUEEXPORT-05');

	await page.getByTestId('queue-select-third-light').click();
	await expect(page.getByTestId('now-playing')).toContainText('Third Light');

	await copy.click();
	await expect.poll(() => clipboardCalls(page)).toHaveLength(1);
	const [written] = await clipboardCalls(page);
	expect(written, 'export must cover the whole queue regardless of selection').toBe(DEFAULT_TEXT);
});

test('[QUEUEEXPORT-06] empty queue disables both actions with an empty-queue reason', async ({
	page
}) => {
	await openQueue(page);
	const { copy, download } = await expectExportButtons(page, 'QUEUEEXPORT-06');

	await page.getByTestId('queue-clear').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');

	await expect(copy, 'copy must disable on an empty queue').toBeDisabled();
	await expect(download, 'download must disable on an empty queue').toBeDisabled();
	await expect(
		page.getByTestId('queue-export-status'),
		'empty queue must explain why the actions are disabled'
	).toContainText(/empty/i);
	expect(await clipboardCalls(page), 'disabled actions must not write the clipboard').toEqual([]);
	expect(
		await page.getByTestId('queue-export-fallback').count(),
		'disabled actions must not show a fallback'
	).toBe(0);

	// Re-adding restores both actions and clears the reason.
	await page.getByTestId('queue-add-demo-track').click();
	await expect(copy, 'copy must re-enable once the queue is non-empty').toBeEnabled();
	await expect(download, 'download must re-enable once the queue is non-empty').toBeEnabled();
	await expect(
		page.getByTestId('queue-export-status'),
		'the empty reason must clear once the queue is non-empty'
	).not.toContainText(/empty/i);
});

test('[QUEUEEXPORT-07] only explicit activation touches the clipboard; no execCommand, backend, or external calls', async ({
	page
}) => {
	const external: string[] = [];
	const api: string[] = [];
	page.on('request', (request) => {
		const url = request.url();
		const host = new URL(url).hostname;
		if (host !== 'localhost' && host !== '127.0.0.1') external.push(url);
		if (url.includes('/api/')) api.push(url);
	});
	await openQueue(page);
	expect(await clipboardCalls(page), 'no clipboard write on load').toEqual([]);
	expect(await execCommandCallCount(page), 'no execCommand on load').toBe(0);

	// Selection, moves, seeks, and timeupdates are not activation.
	await page.getByTestId('queue-select-second-signal').click();
	await page.getByTestId('queue-move-up-second-signal').click();
	await setCurrentTime(page, 33);
	await page
		.getByTestId('demo-audio')
		.evaluate((el) => el.dispatchEvent(new Event('timeupdate')));
	expect(await clipboardCalls(page), 'no clipboard write on change/move/seek').toEqual([]);

	const { copy } = await expectExportButtons(page, 'QUEUEEXPORT-07');
	api.length = 0;
	await copy.click();
	await expect.poll(() => clipboardCalls(page)).toHaveLength(1);
	expect(await execCommandCallCount(page), 'activation must not use execCommand').toBe(0);
	expect(api, 'copy must not contact a backend').toEqual([]);
	expect(external, 'copy must not call external services').toEqual([]);
});

test('[QUEUEEXPORT-08] copy success announces a concise polite confirmation and keeps focus stable', async ({
	page
}) => {
	await openQueue(page);
	const { copy } = await expectExportButtons(page, 'QUEUEEXPORT-08');

	await copy.click();
	const status = page.getByTestId('queue-export-status');
	await expect(status, 'success must announce a confirmation').toContainText(/copied/i);
	const text = (await status.textContent()) ?? '';
	expect(text.length, `confirmation must be concise (actual ${text.length} chars)`).toBeLessThan(100);
	const politeness = await status.evaluate((el) => ({
		live: el.getAttribute('aria-live'),
		role: el.getAttribute('role')
	}));
	expect(
		politeness.live === 'polite' || politeness.role === 'status',
		`status must be polite (actual aria-live=${politeness.live} role=${politeness.role})`
	).toBe(true);

	const focused = await page.evaluate(
		() => (document.activeElement as HTMLElement | null)?.getAttribute('data-testid')
	);
	expect(focused, 'focus must stay on the copy action after success').toBe('queue-export-copy');
});

test('[QUEUEEXPORT-09] missing Clipboard API shows a labeled readonly fallback with the track list', async ({
	page
}) => {
	await openQueue(page, { clipboard: 'missing' });
	const { copy } = await expectExportButtons(page, 'QUEUEEXPORT-09');

	await copy.click();
	const fallback = page.getByTestId('queue-export-fallback');
	await expect(fallback, 'fallback text must appear when clipboard is absent').toBeVisible();
	const tag = await fallback.evaluate((el) => el.tagName);
	expect(tag === 'INPUT' || tag === 'TEXTAREA', `fallback must be input/textarea (actual ${tag})`).toBe(
		true
	);
	await expect(fallback, 'fallback must be labeled').toHaveAccessibleName(/.+/);
	const readonly = await fallback.evaluate((el) => (el as HTMLInputElement).readOnly);
	expect(readonly, 'fallback must be readonly').toBe(true);
	expect(await fallback.inputValue(), 'fallback must hold the exact track-list text').toBe(
		DEFAULT_TEXT
	);

	const instructions = page.getByTestId('queue-export-instructions');
	await expect(instructions, 'fallback must explain the manual copy').toBeVisible();
	await expect(instructions).toContainText(/copy/i);
	await expect(instructions, 'instructions must name a manual gesture').toContainText(
		/ctrl|cmd|⌘|right-click|long-press|keyboard/i
	);
	await expect(instructions, 'fallback must not claim success').not.toContainText(/copied|success/i);
	await expect(page.getByTestId('queue-export-status')).not.toContainText(/copied|success/i);

	// Focus stays put, but the fallback is focusable and fully selectable.
	const focused = await page.evaluate(
		() => (document.activeElement as HTMLElement | null)?.getAttribute('data-testid')
	);
	expect(focused, 'focus must stay on the copy action when the fallback appears').toBe(
		'queue-export-copy'
	);
	await fallback.focus();
	await fallback.evaluate((el) => (el as HTMLInputElement).select());
	const selection = await fallback.evaluate((el) => ({
		start: (el as HTMLInputElement).selectionStart,
		end: (el as HTMLInputElement).selectionEnd,
		length: (el as HTMLInputElement).value.length
	}));
	expect(selection.start, 'fallback must select from the start').toBe(0);
	expect(selection.end, 'fallback must select through the end').toBe(selection.length);
});

test('[QUEUEEXPORT-10] rejected clipboard write falls back without claiming success', async ({
	page
}) => {
	await openQueue(page, { clipboard: 'rejected' });
	const { copy } = await expectExportButtons(page, 'QUEUEEXPORT-10');

	await copy.click();
	const fallback = page.getByTestId('queue-export-fallback');
	await expect(fallback, 'fallback text must appear when the write rejects').toBeVisible();
	expect(await fallback.inputValue(), 'fallback must hold the exact track-list text').toBe(
		DEFAULT_TEXT
	);
	await expect(page.getByTestId('queue-export-instructions')).toContainText(/copy/i);
	await expect(page.getByTestId('queue-export-instructions')).not.toContainText(/copied|success/i);
	await expect(page.getByTestId('queue-export-status')).not.toContainText(/copied|success/i);
	expect(await execCommandCallCount(page), 'rejection must not use execCommand').toBe(0);
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	expect(copy, 'copy stays enabled after a rejected write').toBeEnabled();
});

test('[QUEUEEXPORT-11] download saves queue.m3u with arranged #EXTINF id-only entries, fully offline', async ({
	page
}, testInfo) => {
	const api: string[] = [];
	page.on('request', (request) => {
		if (request.url().includes('/api/')) api.push(request.url());
	});
	await openQueue(page);
	const { download } = await expectExportButtons(page, 'QUEUEEXPORT-11');

	await page.getByTestId('queue-move-down-demo-track').click();
	expect(await queueOrder(page)).toEqual(['second-signal', 'demo-track', 'third-light']);

	api.length = 0;
	const downloadPromise = page.waitForEvent('download');
	await download.click();
	const file = await downloadPromise;
	expect(file.suggestedFilename(), 'download must be named queue.m3u').toBe('queue.m3u');

	const saved = testInfo.outputPath('queue.m3u');
	await mkdir(dirname(saved), { recursive: true });
	await file.saveAs(saved);
	expect(await readFile(saved, 'utf-8'), 'downloaded bytes must be the arranged M3U').toBe(
		MOVED_DOWN_M3U
	);

	const status = page.getByTestId('queue-export-status');
	await expect(status, 'download must announce a confirmation').toContainText(/downloaded|saved/i);
	const text = (await status.textContent()) ?? '';
	expect(text.length, `confirmation must be concise (actual ${text.length} chars)`).toBeLessThan(100);

	expect(api, 'download must not contact a backend').toEqual([]);
	expect(await execCommandCallCount(page), 'download must not use execCommand').toBe(0);
});

test('[QUEUEEXPORT-12] export bytes carry no URLs, grants, providers, or identity and persist nothing', async ({
	page
}, testInfo) => {
	await openQueue(page);
	const { copy, download } = await expectExportButtons(page, 'QUEUEEXPORT-12');

	const before = await storageSnapshot(page);

	await copy.click();
	await expect.poll(() => clipboardCalls(page)).toHaveLength(1);
	const [written] = await clipboardCalls(page);
	expect(written, 'copied text must be the numbered track list').toBe(DEFAULT_TEXT);

	const downloadPromise = page.waitForEvent('download');
	await download.click();
	const file = await downloadPromise;
	const saved = testInfo.outputPath('queue.m3u');
	await mkdir(dirname(saved), { recursive: true });
	await file.saveAs(saved);
	const m3u = await readFile(saved, 'utf-8');
	expect(m3u, 'downloaded bytes must be the default M3U').toBe(DEFAULT_M3U);

	for (const [label, bytes] of [
		['copied text', written ?? ''],
		['M3U bytes', m3u]
	] as const) {
		for (const leaked of LEAK_TOKENS) {
			expect(bytes, `${label} must not contain "${leaked}"`).not.toContain(leaked);
		}
	}
	expect(await storageSnapshot(page), 'export must not persist anything').toEqual(before);
});

test('[QUEUEEXPORT-13] export never plays, pauses, seeks, changes queue/order/rate, or persists', async ({
	page
}) => {
	await openQueue(page);
	const { copy, download } = await expectExportButtons(page, 'QUEUEEXPORT-13');

	await setCurrentTime(page, 50);
	const before = {
		status: await page.getByTestId('player-status').textContent(),
		queue: await queueOrder(page),
		rate: await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => el.playbackRate),
		time: await currentTime(page),
		storage: await storageSnapshot(page),
		media: await mediaCallCounts(page)
	};

	await copy.click();
	await expect(page.getByTestId('queue-export-status')).toContainText(/copied/i);
	const downloadPromise = page.waitForEvent('download');
	await download.click();
	await downloadPromise;

	expect(await page.getByTestId('player-status').textContent(), 'status must not change').toBe(
		before.status
	);
	expect(await queueOrder(page), 'queue order must not change').toEqual(before.queue);
	expect(
		await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => el.playbackRate),
		'playback rate must not change'
	).toBe(before.rate);
	expect(await currentTime(page), 'export must not seek').toBe(before.time);
	expect(await storageSnapshot(page), 'export must not persist').toEqual(before.storage);
	expect(await mediaCallCounts(page), 'export must not play or pause').toEqual(before.media);
});

test('[QUEUEEXPORT-14] rearranging supersedes a pending copy: late completion stays ignored', async ({
	page
}) => {
	await openQueue(page, { clipboard: 'delayed' });
	const { copy } = await expectExportButtons(page, 'QUEUEEXPORT-14');

	await copy.click();
	await expect(page.getByTestId('queue-export-status')).not.toContainText(/copied/i);

	// Supersede the pending write, then let it complete late: it must be ignored.
	await page.getByTestId('queue-move-down-demo-track').click();
	await resolveDelayedWrite(page);
	await page.waitForTimeout(200);
	await expect(
		page.getByTestId('queue-export-status'),
		'late completion from a superseded action must be ignored'
	).not.toContainText(/copied/i);
	expect(
		await page.getByTestId('queue-export-fallback').count(),
		'no stale fallback from the superseded action'
	).toBe(0);

	// The next action on the new order still works.
	await copy.click();
	await resolveDelayedWrite(page);
	await expect(page.getByTestId('queue-export-status')).toContainText(/copied/i);
	const calls = await clipboardCalls(page);
	expect(calls, 'expected one write per activation').toHaveLength(2);
	expect(calls[1], 'second copy must export the rearranged order').toBe(MOVED_DOWN_TEXT);

	// A further queue change clears the fresh success.
	await page.getByTestId('queue-move-up-demo-track').click();
	await expect(
		page.getByTestId('queue-export-status'),
		'success must clear on queue change'
	).not.toContainText(/copied/i);
});

test('[QUEUEEXPORT-15] fresh mount clears stale success state (teardown)', async ({ page }) => {
	await openQueue(page);
	const { copy } = await expectExportButtons(page, 'QUEUEEXPORT-15');

	await copy.click();
	await expect(page.getByTestId('queue-export-status')).toContainText(/copied/i);

	await page.reload();
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(
		page.getByTestId('queue-export-status'),
		'fresh mount must not resurrect the previous success'
	).not.toContainText(/copied/i);
	expect(
		await page.getByTestId('queue-export-fallback').count(),
		'fresh mount must not resurrect the fallback'
	).toBe(0);
});

test.describe('narrow viewport with touch', () => {
	test.use({ viewport: { width: 360, height: 800 }, hasTouch: true });

	test('[QUEUEEXPORT-16] both actions stay visible and operable on a narrow touch screen', async ({
		page
	}) => {
		await openQueue(page);
		const { copy, download } = await expectExportButtons(page, 'QUEUEEXPORT-16');

		for (const [label, button] of [
			['copy', copy],
			['download', download]
		] as const) {
			const box = await button.boundingBox();
			expect(box, `${label} must have a bounding box`).not.toBeNull();
			expect(box?.width ?? 361, `${label} must fit the 360px viewport`).toBeLessThanOrEqual(360);
		}

		await copy.tap();
		await expect
			.poll(() => clipboardCalls(page), 'tap on copy must write the clipboard')
			.toHaveLength(1);
		const [written] = await clipboardCalls(page);
		expect(written, 'tapped copy must export the arranged queue').toBe(DEFAULT_TEXT);

		const downloadPromise = page.waitForEvent('download');
		await download.tap();
		const file = await downloadPromise;
		expect(file.suggestedFilename(), 'tapped download must be named queue.m3u').toBe('queue.m3u');
	});
});
