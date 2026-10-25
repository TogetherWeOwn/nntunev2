import { expect, test, type Page } from '@playwright/test';
import { choose } from './appearance.helpers.js';

/**
 * Protected Test-Author bank: the real library's arranged queue as readable text.
 * Builder continues this branch with implementation and one combined PR; do not
 * weaken this file or change neighboring acceptance banks. No test-only ready PR.
 *
 * New library hooks: library-queue-copy (native "Copy queue as text" button),
 * library-queue-copy-status (polite status), library-queue-copy-fallback (labelled
 * readonly input/textarea), library-queue-copy-instructions (manual-copy gesture).
 * Empty copy remains visible, disabled, with an accessible empty-queue reason.
 * Reuse formatQueueText: full arranged order at activation, not visible matches,
 * current/upcoming-only, URLs, or a backup. Never add M3U/download here.
 *
 * Copy is explicit, single-flight, and presentation-only. Success follows a
 * resolved write; unavailable/denied clipboard exposes selectable text without
 * execCommand or an automatic retry. Queue/auth revisions and teardown invalidate
 * feedback, not the already-issued browser write. Auth also erases fallback text.
 *
 * Run locally with synthetic routes only, from repository root:
 * CI=1 PUBLIC_API_BASE_URL='' pnpm --dir apps/web exec playwright test \
 *   tests/library-queue-copy.spec.ts --workers=1
 * pnpm --dir apps/web test
 * pnpm --dir apps/web check
 * Formatter companions (existing, unchanged): queueExport.acceptance.test.ts and
 * queueExport.builder.test.ts in src/lib/player. Preserve library-share-link,
 * library-queue-filter/manage/orientation, library-selection-isolation, and all
 * demo queue-export banks. Missing browser/dependencies are NOT behavioral RED.
 */

test.use({ viewport: { width: 1000, height: 800 }, hasTouch: true });
test.setTimeout(20_000);

const ITEMS = [
	{ id: 'copy-zulu', title: 'Zulu Song', artist: 'North', streamUrl: '/test-audio/copy-zulu' },
	{ id: 'copy-alpha', title: 'Alpha Song', artist: 'Mira', streamUrl: '/test-audio/copy-alpha' },
	{ id: 'copy-mike', title: 'Mike Song', streamUrl: '/test-audio/copy-mike' }
];
const IDS = ['copy-mike', 'copy-zulu', 'copy-alpha'];
const TEXT = '1. Mike Song\n2. Zulu Song — North\n3. Alpha Song — Mira';
const MOVED = '1. Mike Song\n2. Alpha Song — Mira\n3. Zulu Song — North';
const NEXT_ACCOUNT = [{ id: 'copy-bravo', title: 'Bravo Song', artist: 'Next Artist' }];
type Mode = 'resolve' | 'reject' | 'missing' | 'pending';
type Item = { id: string; title: string; artist?: string; streamUrl?: string; [key: string]: unknown };
type Payload = { items: Item[]; status: number };
type Probe = {
	writes: string[];
	pending: { resolve: () => void; reject: () => void }[];
	media: string[];
	storage: string[];
	exec: number;
	mount: string;
};
declare global {
	interface Window { libraryQueueCopyProbe: Probe }
}

const copy = (page: Page) => page.getByTestId('library-queue-copy');
const status = (page: Page) => page.getByTestId('library-queue-copy-status');
const fallback = (page: Page) => page.getByTestId('library-queue-copy-fallback');
const instructions = (page: Page) => page.getByTestId('library-queue-copy-instructions');
const queueRow = (page: Page, id: string) => page.locator(`[data-testid="queue-item"][data-track-id="${id}"]`);

function receipt(caseId: string, expected: string, actual: unknown, errorCode: string) {
	return JSON.stringify({ case_id: caseId, expected, actual, status: 'RED', error_code: errorCode,
		next_action: 'Builder wires the real library queue copy behavior; preserve this acceptance bank.' });
}

async function openLibrary(page: Page, baseURL: string, mode: Mode = 'resolve', items: Item[] = ITEMS) {
	const payload: Payload = { items, status: 200 };
	const traffic: string[] = [];
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(error.message));
	const origin = new URL(baseURL).origin;
	expect(['localhost', '127.0.0.1', '[::1]']).toContain(new URL(origin).hostname);
	await page.route('**/*', async (route) => {
		const request = route.request();
		const url = new URL(request.url());
		if (url.origin !== origin || /^\/(?:api|v1)(?:\/|$)/.test(url.pathname) || request.resourceType() === 'media') {
			traffic.push(`${request.method()} ${url.origin}${url.pathname}`);
			if (url.origin === origin && url.pathname === '/api/library' && request.method() === 'GET') {
				return route.fulfill({ status: payload.status, contentType: 'application/json', body: JSON.stringify(payload.items) });
			}
			return route.abort();
		}
		return route.continue();
	});
	await page.addInitScript((mode: Mode) => {
		const probe: Probe = { writes: [], pending: [], media: [], storage: [], exec: 0, mount: crypto.randomUUID() };
		window.libraryQueueCopyProbe = probe;
		Object.defineProperty(navigator, 'clipboard', { configurable: true, value: mode === 'missing' ? undefined : {
			writeText(text: string) {
				probe.writes.push(text);
				if (mode === 'reject') return Promise.reject(new DOMException('Denied', 'NotAllowedError'));
				if (mode === 'pending') return new Promise<void>((resolve, reject) => {
					probe.pending.push({ resolve, reject: () => reject(new DOMException('Denied', 'NotAllowedError')) });
				});
				return Promise.resolve();
			}
		} });
		document.execCommand = () => { probe.exec += 1; return false; };
		for (const method of ['play', 'pause', 'load'] as const) {
			Object.defineProperty(HTMLMediaElement.prototype, method, { configurable: true, value() {
				probe.media.push(method);
				return method === 'play' ? Promise.resolve() : undefined;
			} });
		}
		for (const method of ['setItem', 'removeItem', 'clear'] as const) {
			const native = Storage.prototype[method];
			Object.defineProperty(Storage.prototype, method, { configurable: true, value(...args: unknown[]) {
				probe.storage.push(method);
				return Reflect.apply(native, this, args);
			} });
		}
	}, mode);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(items.length);
	return { payload, traffic, errors };
}

async function select(page: Page, id: string) {
	await page.locator(`[data-testid="library-item"][data-id="${id}"] button.nt-library-row`).click();
}
async function seed(page: Page) {
	for (const id of IDS) await select(page, id);
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	expect(await order(page)).toEqual(IDS);
}
async function order(page: Page) {
	return page.getByTestId('queue-item').evaluateAll((rows) => rows.map((row) => row.getAttribute('data-track-id')));
}
async function writes(page: Page) {
	return page.evaluate(() => window.libraryQueueCopyProbe.writes);
}
async function settleUI(page: Page) {
	await page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
}
async function settleWrite(page: Page, index = 0, reject = false) {
	await page.evaluate(({ index, reject }) => {
		const pending = window.libraryQueueCopyProbe.pending[index];
		if (!pending) throw new Error('Expected an activated pending clipboard write');
		if (reject) pending.reject(); else pending.resolve();
	}, { index, reject });
	await settleUI(page);
}
async function requireCopy(page: Page, caseId: string) {
	await expect(copy(page), receipt(caseId, 'one visible Copy queue as text action beside the library queue',
		await copy(page).count(), 'queue_copy_control_missing')).toBeVisible({ timeout: 1500 });
	await expect(copy(page)).toHaveJSProperty('tagName', 'BUTTON');
	await expect(copy(page)).toHaveAccessibleName('Copy queue as text');
	return copy(page);
}
async function quietFeedback(page: Page) {
	// Allow status to unmount or remain an empty polite region.
	expect((await status(page).allTextContents()).join('').trim()).toBe('');
	await expect(fallback(page)).toHaveCount(0);
	await expect(instructions(page)).toHaveCount(0);
}
async function succeeded(page: Page) {
	await expect(status(page)).toContainText(/copied/i);
	expect(await status(page).evaluate((element) => element.getAttribute('aria-live') === 'polite' || element.getAttribute('role') === 'status')).toBe(true);
	await expect(fallback(page)).toHaveCount(0);
	await expect(instructions(page)).toHaveCount(0);
}
async function manual(page: Page, expected = TEXT) {
	await expect(fallback(page)).toBeVisible();
	await expect(fallback(page)).toHaveAccessibleName(/queue.*text|text.*queue/i);
	await expect(fallback(page)).toHaveJSProperty('readOnly', true);
	await expect(fallback(page)).toHaveValue(expected);
	await expect(instructions(page)).toContainText(/copy/i);
	await expect(instructions(page)).toContainText(/ctrl|control|cmd|command|right.click|select/i);
	expect((await status(page).allTextContents()).join(' ')).not.toMatch(/\bcopied\b|cancelled|recalled/i);
	expect(await page.evaluate(() => window.libraryQueueCopyProbe.exec)).toBe(0);
}
async function snapshot(page: Page) {
	return page.evaluate(() => {
		const dump = (storage: Storage) => Object.fromEntries(Object.keys(storage).sort().map((key) => [key, storage.getItem(key)]));
		const audio = document.querySelector<HTMLAudioElement>('[data-testid="audio-player"]');
		return {
			queue: [...document.querySelectorAll('[data-testid="queue-item"]')].map((row) => [row.getAttribute('data-track-id'), row.getAttribute('aria-current')]),
			media: audio ? { time: audio.currentTime, rate: audio.playbackRate, src: audio.getAttribute('src'), paused: audio.paused } : null,
			local: dump(localStorage), session: dump(sessionStorage)
		};
	});
}
async function resetEffects(page: Page) {
	await settleUI(page);
	await page.evaluate(() => {
		window.libraryQueueCopyProbe.media.length = 0;
		window.libraryQueueCopyProbe.storage.length = 0;
	});
}
async function authTransition(page: Page) {
	const tab = await page.context().newPage();
	// Same-origin inert response: this tab's only purpose is a real storage event.
	await tab.route('**/*', (route) => route.fulfill({ contentType: 'text/html', body: '<!doctype html><title>Auth fixture</title>' }));
	await tab.goto(new URL('/login', page.url()).href);
	await tab.evaluate(() => {
		localStorage.setItem('nntune.library.auth-transition.v1', 'synthetic-account-change');
		localStorage.removeItem('nntune.library.auth-transition.v1');
	});
	await tab.close();
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
}
async function navigate(page: Page, href: string) {
	await page.evaluate((href) => {
		const link = document.createElement('a');
		link.href = href;
		document.body.append(link);
		link.click();
		link.remove();
	}, href);
	await page.waitForURL((url) => url.pathname === href);
}

test('LQC-01 empty queue explains why copy is disabled and never writes', async ({ page, baseURL }) => {
	await openLibrary(page, baseURL!);
	const button = await requireCopy(page, 'LQC-01');
	await expect(button).toBeDisabled();
	await expect(button).toHaveAccessibleDescription(/empty|add.*track|select.*track/i);
	await button.dispatchEvent('click');
	expect(await writes(page)).toEqual([]);
	await quietFeedback(page);
});

test('LQC-02 keyboard activation copies arranged order including tracks before current', async ({ page, baseURL }) => {
	await openLibrary(page, baseURL!);
	await seed(page);
	await queueRow(page, 'copy-alpha').getByRole('button', { name: 'Move Alpha Song up', exact: true }).click();
	await page.getByTestId('player-next').click();
	await expect(queueRow(page, 'copy-alpha')).toHaveAttribute('aria-current', 'true');
	await requireCopy(page, 'LQC-02');
	await copy(page).focus();
	await page.keyboard.press('Tab');
	await page.keyboard.press('Shift+Tab');
	await expect(copy(page)).toBeFocused();
	expect(await copy(page).evaluate((element) => {
		const css = getComputedStyle(element);
		return element.matches(':focus-visible') && ((css.outlineStyle !== 'none' && parseFloat(css.outlineWidth) > 0) || css.boxShadow !== 'none');
	})).toBe(true);
	await page.keyboard.press('Enter');
	await expect.poll(() => writes(page)).toEqual([MOVED]);
	await succeeded(page);
	await expect(copy(page)).toBeFocused();
	await page.keyboard.press('Space');
	await expect.poll(() => writes(page)).toEqual([MOVED, MOVED]);
});

test('LQC-03 library and queue filters cannot truncate copied text', async ({ page, baseURL }) => {
	await openLibrary(page, baseURL!);
	await seed(page);
	await page.getByLabel('Filter queue', { exact: true }).fill('Zulu');
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	await page.getByRole('searchbox', { name: 'Find in loaded tracks', exact: true }).fill('Alpha');
	await expect(page.getByTestId('library-item')).toHaveCount(1);
	await (await requireCopy(page, 'LQC-03')).click();
	await expect.poll(() => writes(page)).toEqual([TEXT]);
	await page.getByLabel('Filter queue', { exact: true }).fill('no-queue-matches');
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(copy(page)).toBeEnabled();
	await copy(page).click();
	await expect.poll(() => writes(page)).toEqual([TEXT, TEXT]);
});

test('LQC-04 control characters, blank titles, Unicode and markup remain safe literal display text', async ({ page, baseURL }) => {
	const items = [
		{ id: 'copy-control', title: 'One\nTwo\r\nThree\tEnd', artist: 'A\0B\x01C\x7fD' },
		{ id: 'copy-blank', title: ' \n\0 ', artist: ' \t ' },
		{ id: 'copy-unicode', title: 'Björk 🦢 日本語', artist: 'Sigur Rós' },
		{ id: 'copy-markup', title: '<b>Literal & Song</b>', artist: '<i>Artist</i>' }
	];
	await openLibrary(page, baseURL!, 'resolve', items);
	for (const item of items) await select(page, item.id);
	await (await requireCopy(page, 'LQC-04')).click();
	await expect.poll(() => writes(page)).toEqual(['1. One Two Three End — A B C D\n2. copy-blank\n3. Björk 🦢 日本語 — Sigur Rós\n4. <b>Literal & Song</b> — <i>Artist</i>']);
	await expect(page.getByTestId('queue-list').locator('b, i')).toHaveCount(0);
});

test('LQC-05 only display metadata leaves the queue; URLs, account and grant fields never do', async ({ page, baseURL }) => {
	await openLibrary(page, baseURL!, 'resolve', [{ id: 'copy-private', title: 'Public Title', artist: 'Public Artist',
		streamUrl: '/test-audio/private?grant=synthetic-grant', signedGrant: 'synthetic-grant',
		provider: 'synthetic-provider', accountId: 'synthetic-account', authContext: 'synthetic-context',
		mediaBytes: 'synthetic-bytes', artworkUrl: '/test-art/private' }]);
	await select(page, 'copy-private');
	await (await requireCopy(page, 'LQC-05')).click();
	await expect.poll(() => writes(page)).toEqual(['1. Public Title — Public Artist']);
});

test('LQC-06 pending write is not success; resolved write reports politely without focus theft', async ({ page, baseURL }) => {
	await openLibrary(page, baseURL!, 'pending');
	await seed(page);
	await (await requireCopy(page, 'LQC-06')).click();
	expect(await writes(page)).toEqual([TEXT]);
	expect((await status(page).allTextContents()).join(' ')).not.toMatch(/copied/i);
	await expect(fallback(page)).toHaveCount(0);
	await settleWrite(page);
	await succeeded(page);
	await expect(copy(page)).toBeFocused();
	await expect(page.locator('main [aria-live="assertive"]')).toHaveCount(0);
});

for (const [caseId, mode] of [['LQC-07', 'missing'], ['LQC-08', 'reject']] as const) {
	test(`${caseId} ${mode} clipboard offers readonly selectable text, never a false success or second write`, async ({ page, baseURL }) => {
		await openLibrary(page, baseURL!, mode);
		await seed(page);
		await (await requireCopy(page, caseId)).click();
		await manual(page);
		await expect(copy(page)).toBeFocused();
		await fallback(page).focus();
		await page.keyboard.press('Control+a');
		expect(await fallback(page).evaluate((element: HTMLTextAreaElement | HTMLInputElement) => [element.selectionStart, element.selectionEnd])).toEqual([0, TEXT.length]);
		await page.keyboard.press('Space');
		await expect(fallback(page)).toHaveValue(TEXT);
		await settleUI(page);
		expect(await writes(page)).toEqual(mode === 'missing' ? [] : [TEXT]);
	});
}

test('LQC-09 pending operation prevents repeated click/Enter/Space writes', async ({ page, baseURL }) => {
	await openLibrary(page, baseURL!, 'pending');
	await seed(page);
	const button = await requireCopy(page, 'LQC-09');
	await button.click();
	await button.dispatchEvent('click');
	await page.keyboard.press('Enter');
	await page.keyboard.press('Space');
	await settleUI(page);
	expect(await writes(page)).toEqual([TEXT]);
	await settleWrite(page);
	await expect(button).toBeEnabled();
	await button.click();
	expect(await writes(page)).toEqual([TEXT, TEXT]);
	await settleWrite(page, 1);
	await succeeded(page);
});

for (const [caseId, reject] of [['LQC-10', false], ['LQC-11', true]] as const) {
	test(`${caseId} rearrangement invalidates late ${reject ? 'rejection' : 'success'} but cannot recall the activated text`, async ({ page, baseURL }) => {
		await openLibrary(page, baseURL!, 'pending');
		await seed(page);
		await (await requireCopy(page, caseId)).click();
		await queueRow(page, 'copy-alpha').getByRole('button', { name: 'Move Alpha Song up', exact: true }).click();
		expect(await order(page)).toEqual(['copy-mike', 'copy-alpha', 'copy-zulu']);
		await settleWrite(page, 0, reject);
		expect(await writes(page)).toEqual([TEXT]);
		await quietFeedback(page);
		await expect(copy(page)).toBeEnabled();
		await copy(page).click();
		expect(await writes(page)).toEqual([TEXT, MOVED]);
		await settleWrite(page, 1);
		await succeeded(page);
	});
}

for (const [caseId, mutation] of [['LQC-12', 'remove'], ['LQC-13', 'clear'], ['LQC-14', 'current']] as const) {
	test(`${caseId} ${mutation} queue change suppresses stale fallback`, async ({ page, baseURL }) => {
		await openLibrary(page, baseURL!, 'pending');
		await seed(page);
		await (await requireCopy(page, caseId)).click();
		if (mutation === 'remove') await queueRow(page, 'copy-alpha').getByRole('button', { name: 'Remove Alpha Song from queue', exact: true }).click();
		else if (mutation === 'clear') await page.getByRole('button', { name: 'Clear queue', exact: true }).click();
		else await page.getByTestId('player-next').click();
		await settleWrite(page, 0, true);
		await quietFeedback(page);
		expect(await writes(page)).toEqual([TEXT]);
		if (mutation === 'clear') await expect(copy(page)).toBeDisabled();
	});
}

for (const [caseId, reject] of [['LQC-15', false], ['LQC-16', true]] as const) {
	test(`${caseId} auth switch suppresses old-account ${reject ? 'fallback' : 'success'} and next account starts clean`, async ({ page, baseURL }) => {
		const { payload, errors } = await openLibrary(page, baseURL!, 'pending');
		await seed(page);
		await (await requireCopy(page, caseId)).click();
		await authTransition(page);
		await settleWrite(page, 0, reject);
		await quietFeedback(page);
		payload.items = NEXT_ACCOUNT;
		await page.getByRole('button', { name: 'Retry', exact: true }).click();
		await expect(page.getByTestId('library-item')).toHaveCount(1);
		await select(page, 'copy-bravo');
		await copy(page).click();
		expect(await writes(page)).toEqual([TEXT, '1. Bravo Song — Next Artist']);
		await settleWrite(page, 1);
		await succeeded(page);
		expect(errors).toEqual([]);
	});
}

for (const [caseId, boundary] of [['LQC-17', 'auth-event'], ['LQC-18', '401']] as const) {
	test(`${caseId} ${boundary} erases already-visible manual-copy text before Retry`, async ({ page, baseURL }) => {
		const { payload } = await openLibrary(page, baseURL!, 'reject');
		await seed(page);
		await (await requireCopy(page, caseId)).click();
		await manual(page);
		if (boundary === 'auth-event') await authTransition(page);
		else {
			payload.status = 401;
			await page.getByRole('button', { name: 'Refresh', exact: true }).click();
			await expect(page.getByRole('alert')).toContainText(/signed out|do not have access/i);
			await expect(page.getByTestId('queue-item')).toHaveCount(0);
		}
		await quietFeedback(page);
		// Inspect all mounted form values, not merely a hidden test id.
		expect(await page.locator('input, textarea').evaluateAll((elements) => elements.map((element) => (element as HTMLInputElement).value))).not.toContain(TEXT);
		payload.status = 200;
		payload.items = NEXT_ACCOUNT;
		await page.getByRole('button', { name: 'Retry', exact: true }).click();
		await expect(page.getByTestId('library-item')).toHaveCount(1);
		await quietFeedback(page);
	});
}

for (const [caseId, reject] of [['LQC-19', false], ['LQC-20', true]] as const) {
	test(`${caseId} same-document teardown ignores pending ${reject ? 'rejection' : 'resolution'} and remount is clean`, async ({ page, baseURL }) => {
		const { errors } = await openLibrary(page, baseURL!, 'pending');
		await seed(page);
		await (await requireCopy(page, caseId)).click();
		const mount = await page.evaluate(() => window.libraryQueueCopyProbe.mount);
		await navigate(page, '/');
		expect(await page.evaluate(() => window.libraryQueueCopyProbe.mount)).toBe(mount);
		await settleWrite(page, 0, reject);
		await quietFeedback(page);
		await navigate(page, '/library');
		await expect(page.getByTestId('library-item')).toHaveCount(3);
		await quietFeedback(page);
		expect(await writes(page)).toEqual([TEXT]);
		expect(errors).toEqual([]);
	});
}

for (const [caseId, mode] of [['LQC-21', 'resolve'], ['LQC-22', 'reject']] as const) {
	test(`${caseId} ${mode} copy has zero queue, media, API or storage side effects`, async ({ page, baseURL }) => {
		const { traffic } = await openLibrary(page, baseURL!, mode);
		await seed(page);
		await page.getByTestId('audio-player').evaluate((audio: HTMLAudioElement) => { audio.currentTime = 12; audio.playbackRate = 1.25; });
		await requireCopy(page, caseId);
		await resetEffects(page);
		const before = await snapshot(page);
		traffic.length = 0;
		await copy(page).click();
		if (mode === 'resolve') await succeeded(page); else await manual(page);
		await settleUI(page);
		expect(await snapshot(page)).toEqual(before);
		expect(traffic).toEqual([]);
		const effects = await page.evaluate(() => ({ media: window.libraryQueueCopyProbe.media, storage: window.libraryQueueCopyProbe.storage, exec: window.libraryQueueCopyProbe.exec }));
		expect(effects).toEqual({ media: [], storage: [], exec: 0 });
		expect(await writes(page)).toEqual([TEXT]);
	});
}

test('LQC-23 load, selection, filtering and rearrangement never trigger unsolicited writes', async ({ page, baseURL }) => {
	await openLibrary(page, baseURL!);
	await seed(page);
	await requireCopy(page, 'LQC-23');
	await page.getByLabel('Filter queue', { exact: true }).fill('Alpha');
	await queueRow(page, 'copy-alpha').getByRole('button', { name: 'Move Alpha Song up', exact: true }).click();
	await page.getByLabel('Filter queue', { exact: true }).fill('');
	await page.getByTestId('player-next').click();
	await settleUI(page);
	expect(await writes(page)).toEqual([]);
	await quietFeedback(page);
});

for (const [caseId, palette] of [['LQC-24', 'light'], ['LQC-25', 'dark']] as const) {
	test(`${caseId} narrow ${palette} touch copy/fallback fits without losing selectable text`, async ({ page, baseURL }) => {
		await page.setViewportSize({ width: 320, height: 800 });
		await openLibrary(page, baseURL!, 'reject');
		await choose(page, palette);
		await seed(page);
		const button = await requireCopy(page, caseId);
		const box = await button.boundingBox();
		expect(box).not.toBeNull();
		expect(box!.height).toBeGreaterThanOrEqual(44);
		expect(box!.width).toBeGreaterThanOrEqual(44);
		expect(box!.x).toBeGreaterThanOrEqual(0);
		expect(box!.x + box!.width).toBeLessThanOrEqual(320);
		await button.tap();
		await manual(page);
		const field = await fallback(page).boundingBox();
		expect(field).not.toBeNull();
		expect(field!.x).toBeGreaterThanOrEqual(0);
		expect(field!.x + field!.width).toBeLessThanOrEqual(320);
		expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
		await fallback(page).focus();
		await page.keyboard.press('Control+a');
		expect(await fallback(page).evaluate((element: HTMLTextAreaElement | HTMLInputElement) => [element.selectionStart, element.selectionEnd])).toEqual([0, TEXT.length]);
	});
}

test('LQC-26 already-visible fallback is discarded when its queue snapshot changes', async ({ page, baseURL }) => {
	await openLibrary(page, baseURL!, 'reject');
	await seed(page);
	await (await requireCopy(page, 'LQC-26')).click();
	await manual(page);
	await queueRow(page, 'copy-alpha').getByRole('button', { name: 'Move Alpha Song up', exact: true }).click();
	await quietFeedback(page);
	await copy(page).click();
	await manual(page, MOVED);
	expect(await writes(page)).toEqual([TEXT, MOVED]);
});

test('LQC-27 denied clipboard renders markup-looking text as a value, not elements', async ({ page, baseURL }) => {
	await openLibrary(page, baseURL!, 'reject', [{ id: 'copy-literal', title: '<b>Literal & Song</b>', artist: '<i>Artist</i>' }]);
	await select(page, 'copy-literal');
	await (await requireCopy(page, 'LQC-27')).click();
	await manual(page, '1. <b>Literal & Song</b> — <i>Artist</i>');
	await expect(page.locator('main b, main i')).toHaveCount(0);
});

test('LQC-28 appending to a pending snapshot invalidates fallback and next activation includes the new track', async ({ page, baseURL }) => {
	await openLibrary(page, baseURL!, 'pending');
	await select(page, 'copy-mike');
	await (await requireCopy(page, 'LQC-28')).click();
	await select(page, 'copy-alpha');
	await settleWrite(page, 0, true);
	await quietFeedback(page);
	await copy(page).click();
	expect(await writes(page)).toEqual(['1. Mike Song', '1. Mike Song\n2. Alpha Song — Mira']);
	await settleWrite(page, 1);
	await succeeded(page);
});

for (const [caseId, reject] of [['LQC-29', false], ['LQC-30', true]] as const) {
	test(`${caseId} 401 invalidates pending ${reject ? 'fallback' : 'success'} before the next session`, async ({ page, baseURL }) => {
		const { payload } = await openLibrary(page, baseURL!, 'pending');
		await seed(page);
		await (await requireCopy(page, caseId)).click();
		payload.status = 401;
		await page.getByRole('button', { name: 'Refresh', exact: true }).click();
		await expect(page.getByRole('alert')).toContainText(/signed out|do not have access/i);
		await expect(page.getByTestId('queue-item')).toHaveCount(0);
		await settleWrite(page, 0, reject);
		await quietFeedback(page);
		payload.status = 200;
		payload.items = NEXT_ACCOUNT;
		await page.getByRole('button', { name: 'Retry', exact: true }).click();
		await expect(page.getByTestId('library-item')).toHaveCount(1);
		await quietFeedback(page);
		expect(await writes(page)).toEqual([TEXT]);
	});
}

test('LQC-31 a queue revision does not allow a second write while the first browser write is unsettled', async ({ page, baseURL }) => {
	await openLibrary(page, baseURL!, 'pending');
	await seed(page);
	await (await requireCopy(page, 'LQC-31')).click();
	await queueRow(page, 'copy-alpha').getByRole('button', { name: 'Move Alpha Song up', exact: true }).click();
	await copy(page).dispatchEvent('click');
	await settleUI(page);
	expect(await writes(page)).toEqual([TEXT]);
	await settleWrite(page);
	await quietFeedback(page);
	await copy(page).click();
	expect(await writes(page)).toEqual([TEXT, MOVED]);
	await settleWrite(page, 1);
	await succeeded(page);
});
