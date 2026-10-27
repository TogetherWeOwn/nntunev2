import { expect, test, type Page } from '@playwright/test';

/**
 * Test-Author owned RED acceptance for LIBQUEUESHARE: copy a link to any
 * queued track without leaving the queue. The Builder wires the library
 * queue rows to satisfy this file and must not edit it.
 *
 * LOCKED PAGE CONTRACT (new hooks on `/library`; existing queue rows,
 * browse rows, favorites, filters, order and inbound-link hooks unchanged):
 * - Every `queue-item` row carries a NATIVE <button>
 *   `queue-copy-link` whose accessible name matches /copy link|share/i and
 *   names the row's track title (per-track naming, like the queue star
 *   toggles). Keyboard reachable with visible focus (:focus-visible +
 *   outline/box-shadow); Enter/Space activates the focused action.
 * - Activating a row's copy writes ONE fresh same-origin URL: origin +
 *   `/library?` + buildShareQuery(trackId, seconds) (only `track` + `t`;
 *   floor + encodeURIComponent semantics), where trackId is that row's
 *   stable `data-track-id`. The position is the CURRENT audio element's
 *   currentTime read live at activation when the row's track is the
 *   current track with usable media; otherwise t=0 (a non-current row is
 *   never stamped with another track's position). Never clones the
 *   current search/hash and never uses a stream URL: token, session,
 *   grant, bearer, provider, account email, fragment and /test-audio
 *   material must not enter the result.
 * - `queue-share-status`: one shared aria-live="polite" region (or
 *   role="status") scoped to queue-share outcomes. On a fulfilled write
 *   it announces a concise (<100 chars) confirmation matching /copied/i
 *   and naming the copied track. On fallback/refusal it never claims
 *   success. When the row is gone before completion (removal, refresh,
 *   auth change) it announces honest unavailable feedback matching
 *   /unavailable|no longer|removed|not in (the )?queue/i. Cleared on
 *   queue/selection change and on fresh mount; late completion from a
 *   superseded action is ignored.
 * - `queue-share-fallback`: one shared LABELED readonly <input> or
 *   <textarea> holding the share URL as its value (safe text, never
 *   innerHTML). Shown only when the Clipboard API is absent or rejects
 *   for a still-queued row. Focusable and selectable; focus stays on the
 *   activating row button. Never shown for unavailable rows.
 * - `queue-share-instructions`: actionable manual-copy instructions shown
 *   with the fallback (matches /copy/i, names a manual gesture:
 *   Ctrl/Cmd/right-click/etc.). Never claims success.
 * - Only explicit activation may touch the clipboard: no writes on load,
 *   queueing, selection, move, seek or timeupdate. No
 *   `document.execCommand` calls, no new fetch requests, no unsolicited
 *   clipboard attempts.
 * - Copy and fallback never play, pause, seek, change selection, queue
 *   order, queue/library filters, order, view, favorites or rate, and
 *   never persist anything (localStorage and sessionStorage unchanged
 *   apart from pre-existing load writes). No audio element is summoned
 *   or started by sharing.
 * - Announcements stay scoped: a queue share updates only
 *   `queue-share-status`; `queue-announcement`, `library-share-status`,
 *   `favorites-announcement` and now-playing text are untouched.
 *
 * PROTECTED (Test-Author owned; Builder must not edit):
 * - `apps/web/tests/queue-row-share.spec.ts` (this file)
 * BUILDER-OWNED implementation surface:
 * - `apps/web/src/lib/player/PlayerQueue.svelte` (per-row action)
 * - `apps/web/src/routes/library/+page.svelte` (share logic + feedback)
 * - Existing share/seek/queue suites stay unchanged.
 *
 * Run from the repository root (Node 24 / pnpm 10); builds + serves
 * locally, all APIs fulfilled in-test, no staging/prod access:
 *   CI=1 pnpm --dir apps/web test:e2e tests/queue-row-share.spec.ts --workers=1
 * Do not insert `--` after these pnpm scripts: this toolchain forwards it,
 * ending option parsing and selecting the entire suite. Ensure preview
 * port 4173 is free; never reuse another worktree's preview.
 *
 * RED NOW: `queue-copy-link` does not exist, so every case below fails at
 * its button assertion with a NEXT action. No import/compile failure
 * counts as RED: this file imports only `@playwright/test`.
 * Every RED assertion message is a compact JSON receipt:
 * {"case","expected","actual","status","error_code","next_action"}.
 */

const ITEMS = [
	{ id: 'qshare-coral', title: 'Coral Night', artist: 'Mira Vale', streamUrl: '/test-audio/qshare-coral' },
	{ id: 'qshare-pine', title: 'Pine Morning', artist: 'North Field', streamUrl: '/test-audio/qshare-pine' },
	{ id: 'qshare-ember', title: 'Ember Line', artist: 'Ash Road', streamUrl: '/test-audio/qshare-ember' }
];

const COPY_LABEL = /copy link|share/i;
const UNAVAILABLE_LABEL = /unavailable|no longer|removed|not in (the )?queue/i;

type ClipboardMode = 'fulfilled' | 'rejected' | 'missing' | 'delayed';

interface QueueItem {
	id: string;
	title: string;
	artist?: string;
	streamUrl?: string;
}

/** Compact JSON receipt carried by every RED assertion message. */
function note(
	caseId: string,
	expected: string,
	actual: string,
	errorCode: string,
	nextAction: string
): string {
	return JSON.stringify({
		case: caseId,
		expected,
		actual,
		status: 'RED',
		error_code: errorCode,
		next_action: nextAction
	});
}

/** Keep media synthetic and pending; stub clipboard/media per mode. */
async function openLibrary(
	page: Page,
	options: {
		url?: string;
		items?: QueueItem[] | { current: unknown };
		clipboard?: ClipboardMode;
		duration?: number | null;
		email?: string | null;
	} = {}
) {
	const { url = '/library', clipboard = 'fulfilled', duration = null, email = null } = options;
	const rawItems = options.items ?? ITEMS;
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(
				Array.isArray(rawItems) ? rawItems : (rawItems as { current: unknown }).current
			)
		})
	);
	await page.route('**/test-audio/**', () => {});
	await page.addInitScript(
		({
			mode,
			mockedDuration,
			stashedEmail
		}: {
			mode: ClipboardMode;
			mockedDuration: number | null;
			stashedEmail: string | null;
		}) => {
			const calls: string[] = [];
			const control: {
				calls: string[];
				deferred: { resolve: () => void; reject: (error: unknown) => void } | null;
			} = { calls, deferred: null };
			(window as unknown as { __queueShareCalls: string[] }).__queueShareCalls = calls;
			(window as unknown as { __queueShareControl: unknown }).__queueShareControl = control;
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
			(window as unknown as { __queueShareExecCalls: unknown[][] }).__queueShareExecCalls =
				execCalls;
			(document as unknown as { execCommand: (...args: unknown[]) => boolean }).execCommand = (
				...args
			) => {
				execCalls.push(args);
				return false;
			};
			const mediaCalls = { play: 0, pause: 0 };
			(window as unknown as { __queueShareMediaCalls: { play: number; pause: number } }).__queueShareMediaCalls =
				mediaCalls;
			HTMLMediaElement.prototype.play = function () {
				mediaCalls.play += 1;
				return new Promise<void>(() => {});
			};
			HTMLMediaElement.prototype.pause = function () {
				mediaCalls.pause += 1;
			};
			if (mockedDuration !== null) {
				Object.defineProperty(HTMLMediaElement.prototype, 'duration', {
					configurable: true,
					get: () => mockedDuration
				});
			}
			if (stashedEmail !== null) {
				try {
					sessionStorage.setItem('nntune:signed-in-email', stashedEmail);
				} catch {
					// Display-only label; a blocked store just loses it.
				}
			}
		},
		{ mode: clipboard, mockedDuration: duration, stashedEmail: email }
	);
	await page.goto(url);
	const expectedCount = Array.isArray(rawItems)
		? rawItems.length
		: Array.isArray((rawItems as { current: unknown }).current)
			? ((rawItems as { current: unknown }).current as unknown[]).length
			: 0;
	await expect(page.getByTestId('library-item')).toHaveCount(expectedCount);
}

function libraryRow(page: Page, id: string) {
	return page.locator(`[data-testid="library-item"][data-id="${id}"]`);
}

function queueRow(page: Page, id: string) {
	return page.locator(
		`[data-testid="queue-item"][data-track-id="${id}"], [data-testid="queue-item"][data-id="${id}"]`
	);
}

/**
 * Click the library row's select control (the button whose accessible name
 * STARTS with the track title). Star ("Star X") and copy ("Copy link X")
 * buttons share the title substring, so a plain name match would be
 * ambiguous; the `^Title` anchor keeps selection clicks on the select
 * control in both RED and GREEN trees.
 */
async function selectLibraryRow(page: Page, id: string, title: string) {
	const escaped = title.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
	await libraryRow(page, id)
		.getByRole('button', { name: new RegExp(`^${escaped}( — |$)`) })
		.click();
}

/** Queue coral, pine, ember in gesture order; current is coral. */
async function seedQueue(page: Page) {
	await selectLibraryRow(page, 'qshare-coral', 'Coral Night');
	await selectLibraryRow(page, 'qshare-pine', 'Pine Morning');
	await selectLibraryRow(page, 'qshare-ember', 'Ember Line');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(page.getByTestId('now-playing')).toContainText('Coral Night');
}

async function clipboardCalls(page: Page): Promise<string[]> {
	return page.evaluate(
		() => (window as unknown as { __queueShareCalls: string[] }).__queueShareCalls ?? []
	);
}

async function execCommandCallCount(page: Page): Promise<number> {
	return page.evaluate(
		() =>
			((window as unknown as { __queueShareExecCalls: unknown[] }).__queueShareExecCalls ?? [])
				.length
	);
}

async function mediaCallCounts(page: Page): Promise<{ play: number; pause: number }> {
	return page.evaluate(
		() =>
			(window as unknown as { __queueShareMediaCalls: { play: number; pause: number } })
				.__queueShareMediaCalls ?? { play: 0, pause: 0 }
	);
}

async function setAudioTime(page: Page, seconds: number) {
	await page
		.getByTestId('audio-player')
		.evaluate((el, value) => {
			(el as HTMLAudioElement).currentTime = value;
		}, seconds);
}

async function audioTime(page: Page) {
	return page.getByTestId('audio-player').evaluate((el: HTMLAudioElement) => el.currentTime);
}

async function currentQueueId(page: Page): Promise<string | null> {
	return page
		.locator('[data-testid="queue-item"][aria-current="true"]')
		.evaluateAll((els) => els.map((el) => el.getAttribute('data-track-id') ?? el.getAttribute('data-id')))
		.then((ids) => ids[0] ?? null);
}

async function queueOrder(page: Page) {
	return page
		.getByTestId('queue-item')
		.evaluateAll((els) =>
			els.map((el) => el.getAttribute('data-track-id') ?? el.getAttribute('data-id'))
		);
}

async function storageDump(page: Page) {
	return page.evaluate(() => {
		const dump = (storage: Storage) => {
			const out: Record<string, string | null> = {};
			for (let i = 0; i < storage.length; i += 1) {
				const key = storage.key(i);
				if (key !== null) out[key] = storage.getItem(key);
			}
			return out;
		};
		return { local: dump(localStorage), session: dump(sessionStorage) };
	});
}

async function resolveDelayedWrite(page: Page) {
	await page.evaluate(() => {
		const control = (
			window as unknown as {
				__queueShareControl: { deferred: { resolve: () => void } | null };
			}
		).__queueShareControl;
		if (!control.deferred) throw new Error('no deferred clipboard write is pending');
		control.deferred.resolve();
	});
}

/** Fresh same-origin /library URL with exactly track + t (floor semantics). */
function expectShareUrl(urlString: string, expected: { track: string; t: number }, pageUrl: string) {
	const url = new URL(urlString);
	const origin = new URL(pageUrl).origin;
	expect(url.origin, `expected same-origin ${origin} (actual ${url.origin})`).toBe(origin);
	expect(url.pathname, `expected pathname /library (actual ${url.pathname})`).toBe('/library');
	expect(url.hash, `expected no fragment (actual "${url.hash}")`).toBe('');
	expect(
		[...url.searchParams.keys()].sort(),
		`expected only track+t params (actual "${url.search}")`
	).toEqual(['t', 'track']);
	expect(url.searchParams.get('track'), 'expected track param to match').toBe(expected.track);
	expect(url.searchParams.get('t'), 'expected t param to match floored seconds').toBe(
		String(expected.t)
	);
	expect(urlString, 'must never use a stream URL').not.toContain('/test-audio');
}

async function expectQueueCopyButton(page: Page, caseId: string, id: string, title: string) {
	const copy = queueRow(page, id).getByTestId('queue-copy-link');
	await expect(
		copy,
		note(
			caseId,
			`a labeled native Copy button in queue row ${id}`,
			'missing',
			'queue_copy_missing',
			`Builder adds <button data-testid="queue-copy-link">Copy link ${title}</button> to every queue row.`
		)
	).toBeVisible();
	await expect(copy, `${caseId}: expected a native <button>.`).toHaveJSProperty('tagName', 'BUTTON');
	await expect(copy, `${caseId}: expected accessible name matching /copy link|share/i.`).toHaveAccessibleName(
		COPY_LABEL
	);
	await expect(
		copy,
		`${caseId}: expected the accessible name to carry the track title "${title}".`
	).toHaveAccessibleName(new RegExp(title.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'), 'i'));
	return copy;
}

test('[QUEUESHARE-01] queue rows expose labeled native copy actions, keyboard-focusable with visible focus', async ({
	page
}) => {
	await openLibrary(page);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await seedQueue(page);
	const copy = await expectQueueCopyButton(page, 'QUEUESHARE-01', 'qshare-pine', 'Pine Morning');

	// Keyboard reachability + visible focus (modelled on the browse copy case).
	let found = false;
	for (let tab = 0; tab < 120 && !found; tab += 1) {
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
		if (focused?.testid === 'queue-copy-link') {
			found = true;
			expect(focused.focusVisible, 'queue-copy-link must match :focus-visible').toBe(true);
			const visible =
				focused.outlineWidth !== '0px' && focused.outlineWidth !== '0'
					? true
					: focused.boxShadow !== 'none';
			expect(visible, 'queue-copy-link must show a visible focus indicator').toBe(true);
		}
	}
	expect(found, 'QUEUESHARE-01: queue-copy-link must be reachable by Tab').toBe(true);

	// Keyboard operation: Enter activates the copy (non-current row: t=0).
	await copy.focus();
	await page.keyboard.press('Enter');
	await expect
		.poll(() => clipboardCalls(page), 'Enter on queue-copy-link must write the clipboard')
		.toHaveLength(1);
	const [written] = await clipboardCalls(page);
	expectShareUrl(written ?? '', { track: 'qshare-pine', t: 0 }, page.url());
});

test('[QUEUESHARE-02] non-current row copies its stable id at t=0 and changes nothing', async ({
	page
}) => {
	await openLibrary(page);
	await seedQueue(page);
	await setAudioTime(page, 42);
	const copy = await expectQueueCopyButton(page, 'QUEUESHARE-02', 'qshare-pine', 'Pine Morning');

	// A row is never stamped with another track's position.
	await copy.click();
	await expect.poll(() => clipboardCalls(page)).toHaveLength(1);
	const [written] = await clipboardCalls(page);
	expectShareUrl(written ?? '', { track: 'qshare-pine', t: 0 }, page.url());

	// Copying a queued row selects nothing: coral stays current, order holds.
	expect(await queueOrder(page)).toEqual(['qshare-coral', 'qshare-pine', 'qshare-ember']);
	expect(await currentQueueId(page)).toBe('qshare-coral');
	await expect(page.getByTestId('now-playing')).toContainText('Coral Night');
	expect(await audioTime(page), 'copy must not seek').toBe(42);

	const status = page.getByTestId('queue-share-status');
	await expect(status, 'success must announce a confirmation').toContainText(/copied/i);
	await expect(status, 'confirmation must name the copied track').toContainText(/Pine Morning/);
});

test('[QUEUESHARE-03] current-row copy reads live floored seconds; successive copies never go stale', async ({
	page
}) => {
	await openLibrary(page);
	await seedQueue(page);
	const coralCopy = await expectQueueCopyButton(page, 'QUEUESHARE-03', 'qshare-coral', 'Coral Night');

	await setAudioTime(page, 95.9);
	await coralCopy.click();
	await expect.poll(() => clipboardCalls(page)).toHaveLength(1);

	// Change only the media time: the next copy must read it fresh.
	await setAudioTime(page, 50.2);
	await coralCopy.click();
	await expect.poll(() => clipboardCalls(page)).toHaveLength(2);

	// Advance selection, then sample the new current row's position.
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Pine Morning');
	expect(await currentQueueId(page)).toBe('qshare-pine');
	await setAudioTime(page, 5.9);
	const pineCopy = await expectQueueCopyButton(page, 'QUEUESHARE-03', 'qshare-pine', 'Pine Morning');
	await pineCopy.click();
	await expect.poll(() => clipboardCalls(page)).toHaveLength(3);

	// Coral is no longer current: its next copy stamps t=0, not stale time.
	await coralCopy.click();
	await expect.poll(() => clipboardCalls(page)).toHaveLength(4);

	const calls = await clipboardCalls(page);
	expectShareUrl(calls[0] ?? '', { track: 'qshare-coral', t: 95 }, page.url());
	expectShareUrl(calls[1] ?? '', { track: 'qshare-coral', t: 50 }, page.url());
	expectShareUrl(calls[2] ?? '', { track: 'qshare-pine', t: 5 }, page.url());
	expectShareUrl(calls[3] ?? '', { track: 'qshare-coral', t: 0 }, page.url());
});

test('[QUEUESHARE-04] copied URL carries only track/t: no tokens, account details or raw metadata', async ({
	page
}) => {
	// Synthetic token-like fixture values only; none may enter the result.
	const email = 'queue-share-reader@example.test';
	await openLibrary(page, {
		url: '/library?token=SYNTHETIC_TOKEN_123&session=SYNTHETIC_SESSION_abc&grant=signed-grant-xyz&provider=acme#/frag?bearer=SYNTHETIC_BEARER',
		email
	});
	await seedQueue(page);
	const copy = await expectQueueCopyButton(page, 'QUEUESHARE-04', 'qshare-pine', 'Pine Morning');

	await setAudioTime(page, 42.7);
	await copy.click();
	await expect.poll(() => clipboardCalls(page)).toHaveLength(1);
	const [written] = await clipboardCalls(page);
	// Pine is non-current (coral is current), so t=0 despite the media time.
	expectShareUrl(written ?? '', { track: 'qshare-pine', t: 0 }, page.url());

	for (const leaked of [
		'SYNTHETIC_TOKEN_123',
		'SYNTHETIC_SESSION_abc',
		'signed-grant-xyz',
		'acme',
		'SYNTHETIC_BEARER',
		'frag',
		'bearer',
		'token',
		'session',
		'grant',
		'provider',
		email,
		'queue-share-reader',
		'Coral Night',
		'Pine Morning',
		'Ember Line',
		'Mira Vale',
		'North Field',
		'Ash Road',
		'/test-audio/'
	]) {
		expect(written ?? '', `result must not contain fixture material "${leaked}"`).not.toContain(
			leaked
		);
	}
	expect(written ?? '', 'result must not contain a fragment').not.toContain('#');
});

test('[QUEUESHARE-05] missing Clipboard API shows a labeled readonly fallback with instructions', async ({
	page
}) => {
	await openLibrary(page, { clipboard: 'missing' });
	await seedQueue(page);
	const copy = await expectQueueCopyButton(page, 'QUEUESHARE-05', 'qshare-pine', 'Pine Morning');

	await copy.click();
	const fallback = page.getByTestId('queue-share-fallback');
	await expect(fallback, 'fallback URL must appear when clipboard is absent').toBeVisible();
	const tag = await fallback.evaluate((el) => el.tagName);
	expect(tag === 'INPUT' || tag === 'TEXTAREA', `fallback must be input/textarea (actual ${tag})`).toBe(
		true
	);
	await expect(fallback, 'fallback must be labeled').toHaveAccessibleName(/.+/);
	const readonly = await fallback.evaluate((el) => (el as HTMLInputElement).readOnly);
	expect(readonly, 'fallback must be readonly').toBe(true);
	const value = await fallback.inputValue();
	expectShareUrl(value, { track: 'qshare-pine', t: 0 }, page.url());

	const instructions = page.getByTestId('queue-share-instructions');
	await expect(instructions, 'fallback must explain the manual copy').toBeVisible();
	await expect(instructions).toContainText(/copy/i);
	await expect(instructions, 'instructions must name a manual gesture').toContainText(
		/ctrl|cmd|⌘|right-click|long-press|keyboard/i
	);
	await expect(instructions, 'fallback must not claim success').not.toContainText(/copied|success/i);
	await expect(page.getByTestId('queue-share-status')).not.toContainText(/copied|success/i);

	// Focus stays put, but the fallback is focusable and fully selectable.
	const focused = await page.evaluate(() => {
		const el = document.activeElement as HTMLElement | null;
		return {
			testid: el?.getAttribute('data-testid'),
			row: el?.closest('[data-testid="queue-item"]')?.getAttribute('data-track-id')
		};
	});
	expect(focused.testid, 'focus must stay on a copy action when the fallback appears').toBe(
		'queue-copy-link'
	);
	expect(focused.row, 'focus must stay on the activating row').toBe('qshare-pine');
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

test('[QUEUESHARE-06] rejected clipboard write falls back without claiming success', async ({
	page
}) => {
	await openLibrary(page, { clipboard: 'rejected' });
	await seedQueue(page);
	const copy = await expectQueueCopyButton(page, 'QUEUESHARE-06', 'qshare-ember', 'Ember Line');

	await copy.click();
	const fallback = page.getByTestId('queue-share-fallback');
	await expect(fallback, 'fallback URL must appear when the write rejects').toBeVisible();
	expectShareUrl(await fallback.inputValue(), { track: 'qshare-ember', t: 0 }, page.url());
	await expect(page.getByTestId('queue-share-instructions')).toContainText(/copy/i);
	await expect(page.getByTestId('queue-share-instructions')).not.toContainText(/copied|success/i);
	await expect(page.getByTestId('queue-share-status')).not.toContainText(/copied|success/i);
	expect(await execCommandCallCount(page), 'rejection must not use execCommand').toBe(0);
	expect(await queueOrder(page), 'rejection must not change the queue').toEqual([
		'qshare-coral',
		'qshare-pine',
		'qshare-ember'
	]);
	expect(await currentQueueId(page), 'rejection must not change selection').toBe('qshare-coral');
});

test('[QUEUESHARE-07] fallback renders the URL as safe text: no HTML injection from query or hash', async ({
	page
}) => {
	await openLibrary(page, {
		url: '/library?track=qshare-coral&t=5&x=%3Cimg%20src%3Dx%20onerror%3Dalert(1)%3E#%3Cscript%3Ealert(2)%3C%2Fscript%3E',
		clipboard: 'missing'
	});
	await seedQueue(page);
	const copy = await expectQueueCopyButton(page, 'QUEUESHARE-07', 'qshare-coral', 'Coral Night');

	await setAudioTime(page, 10);
	await copy.click();
	const fallback = page.getByTestId('queue-share-fallback');
	await expect(fallback).toBeVisible();
	const value = await fallback.inputValue();
	expectShareUrl(value, { track: 'qshare-coral', t: 10 }, page.url());
	for (const fragment of ['<', '>', 'img', 'script', 'onerror', 'alert']) {
		expect(value, `fallback value must not contain "${fragment}"`).not.toContain(fragment);
	}
	const injected = await page.evaluate(
		() =>
			document.querySelectorAll('img[src="x"]').length +
			Array.from(document.querySelectorAll('script')).filter((el) =>
				(el.textContent ?? '').includes('alert(2)')
			).length
	);
	expect(injected, 'malicious query/hash must not inject elements').toBe(0);
	const outer = await fallback.evaluate((el) => el.outerHTML);
	expect(outer, 'fallback must not embed parsed markup').not.toContain('<img');
	await expect(page.getByTestId('queue-share-instructions')).not.toContainText(/<img|<script/i);
});

test('[QUEUESHARE-08] row removed before copy completes yields unavailable feedback, never a stale success', async ({
	page
}) => {
	await openLibrary(page, { clipboard: 'delayed' });
	await seedQueue(page);
	const copy = await expectQueueCopyButton(page, 'QUEUESHARE-08', 'qshare-pine', 'Pine Morning');

	// A valid activation with a pending clipboard write: no success yet.
	await copy.click();
	await expect(page.getByTestId('queue-share-status')).not.toContainText(/copied/i);

	// Remove the row under the pending write, then let it complete late.
	await queueRow(page, 'qshare-pine')
		.getByRole('button', { name: 'Remove Pine Morning from queue', exact: true })
		.click();
	await expect(queueRow(page, 'qshare-pine')).toHaveCount(0);
	await resolveDelayedWrite(page);
	await page.waitForTimeout(200);

	const status = page.getByTestId('queue-share-status');
	await expect(
		status,
		note(
			'QUEUESHARE-08',
			'honest unavailable feedback for the removed row',
			'missing',
			'queue_share_unavailable_missing',
			'Builder announces "Link unavailable — Pine Morning is no longer in the queue." in queue-share-status when the row is gone before completion.'
		)
	).toContainText(UNAVAILABLE_LABEL);
	await expect(status, 'unavailable must never claim success').not.toContainText(/copied|success/i);
	expect(
		await page.getByTestId('queue-share-fallback').count(),
		'no fallback for an unavailable row'
	).toBe(0);
	expect(
		await clipboardCalls(page),
		'expected only the original pending write, no stale second write'
	).toHaveLength(1);

	// The next action on a surviving row still works.
	const emberCopy = await expectQueueCopyButton(
		page,
		'QUEUESHARE-08',
		'qshare-ember',
		'Ember Line'
	);
	await emberCopy.click();
	await resolveDelayedWrite(page);
	await expect(page.getByTestId('queue-share-status')).toContainText(/copied/i);
	const calls = await clipboardCalls(page);
	expect(calls, 'expected one write per activation').toHaveLength(2);
	expectShareUrl(calls[1] ?? '', { track: 'qshare-ember', t: 0 }, page.url());
});

test('[QUEUESHARE-09] refresh dropping the row before copy completes yields unavailable feedback', async ({
	page
}) => {
	const payload: { current: unknown } = { current: ITEMS };
	await openLibrary(page, { items: payload, clipboard: 'delayed' });
	await seedQueue(page);
	const copy = await expectQueueCopyButton(page, 'QUEUESHARE-09', 'qshare-pine', 'Pine Morning');

	await copy.click();
	await expect(page.getByTestId('queue-share-status')).not.toContainText(/copied/i);

	// Refresh drops pine from the library, pruning it from the queue too.
	payload.current = [ITEMS[0], ITEMS[2]];
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(2);
	await expect.poll(() => queueOrder(page)).toEqual(['qshare-coral', 'qshare-ember']);

	await resolveDelayedWrite(page);
	await page.waitForTimeout(200);
	const status = page.getByTestId('queue-share-status');
	await expect(
		status,
		note(
			'QUEUESHARE-09',
			'honest unavailable feedback after refresh pruned the row',
			'missing',
			'queue_share_refresh_unavailable_missing',
			'Builder announces unavailable in queue-share-status when refresh removes the pending row before completion.'
		)
	).toContainText(UNAVAILABLE_LABEL);
	await expect(status, 'unavailable must never claim success').not.toContainText(/copied|success/i);
	expect(
		await page.getByTestId('queue-share-fallback').count(),
		'no fallback for an unavailable row'
	).toBe(0);
	expect(
		await clipboardCalls(page),
		'expected only the original pending write, no stale second write'
	).toHaveLength(1);
});

test('[QUEUESHARE-10] auth teardown during a pending share clears state; late completion stays ignored', async ({
	page
}) => {
	const payload: { current: unknown } = { current: ITEMS };
	await openLibrary(page, { items: payload, clipboard: 'delayed' });
	await seedQueue(page);
	const copy = await expectQueueCopyButton(page, 'QUEUESHARE-10', 'qshare-pine', 'Pine Morning');

	await copy.click();
	await expect(page.getByTestId('queue-share-status')).not.toContainText(/copied/i);

	// Cross-tab auth transition: the queue is purged at the boundary.
	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate((key) => {
		window.localStorage.setItem(key, 'test-auth-transition');
		window.localStorage.removeItem(key);
	}, 'nntune.library.auth-transition.v1');
	await otherTab.close();

	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);

	// Late completion from the pre-teardown write must be ignored.
	await resolveDelayedWrite(page);
	await page.waitForTimeout(200);
	await expect(
		page.getByTestId('queue-share-status'),
		'late completion after auth teardown must never claim success'
	).not.toContainText(/copied|success/i);
	expect(
		await page.getByTestId('queue-share-fallback').count(),
		'no stale fallback after auth teardown'
	).toBe(0);

	// Retry into the next session resurrects no share state.
	payload.current = [{ id: 'qshare-bravo', title: 'Bravo Song' }];
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expect(page.getByTestId('library-list')).toContainText('Bravo Song');
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(
		page.getByTestId('queue-share-status'),
		'retry must not resurrect pre-teardown share state'
	).not.toContainText(/copied|success/i);
	expect(
		await page.getByTestId('queue-share-fallback').count(),
		'retry must not resurrect the fallback'
	).toBe(0);
});

test('[QUEUESHARE-11] share changes nothing: selection, order, filters, favorites, playback, storage, network, media', async ({
	page
}) => {
	const fetches: string[] = [];
	page.on('request', (request) => {
		fetches.push(request.url());
	});
	const external: string[] = [];
	page.on('request', (request) => {
		const host = new URL(request.url()).hostname;
		if (host !== 'localhost' && host !== '127.0.0.1') external.push(request.url());
	});
	await openLibrary(page);
	await seedQueue(page);
	const copy = await expectQueueCopyButton(page, 'QUEUESHARE-11', 'qshare-pine', 'Pine Morning');

	// Arrange distinctive state the share must preserve. The queue filter
	// 'n' matches every fixture title (Night/Morning/Line) so the full
	// order stays verifiable while the filter itself is non-empty.
	await page.locator('#queue-filter-input').fill('Pine');
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	await page.locator('#queue-filter-input').fill('');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await page.locator('#queue-filter-input').fill('n');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await page.locator('#library-find').fill('Coral');
	await page.locator('#library-find-scope').selectOption('title');
	await page.locator('#library-order').selectOption('title-desc');
	await libraryRow(page, 'qshare-coral').getByRole('button', { name: /star/i }).click();
	await expect(page.getByTestId('favorite-item')).toHaveCount(1);
	await setAudioTime(page, 50);
	// Pine stays visible under the shared filter so its copy stays clickable.
	await expect(queueRow(page, 'qshare-pine')).toBeVisible();

	const before = {
		queue: await queueOrder(page),
		current: await currentQueueId(page),
		position: await page.getByTestId('queue-position').textContent(),
		nowPlaying: await page.getByTestId('now-playing').textContent(),
		queueFilter: await page.locator('#queue-filter-input').inputValue(),
		find: await page.locator('#library-find').inputValue(),
		scope: await page.locator('#library-find-scope').inputValue(),
		order: await page.locator('#library-order').inputValue(),
		favorites: await page
			.getByTestId('favorite-item')
			.evaluateAll((els) => els.map((el) => el.getAttribute('data-id'))),
		favoritesAnnouncement: await page.getByTestId('favorites-announcement').textContent(),
		queueAnnouncement: await page.getByTestId('queue-announcement').textContent(),
		browseStatus: await page.getByTestId('library-share-status').textContent(),
		rate: await page.getByTestId('audio-player').evaluate((el: HTMLAudioElement) => el.playbackRate),
		time: await audioTime(page),
		audioCount: await page.getByTestId('audio-player').count(),
		storage: await storageDump(page),
		media: await mediaCallCounts(page)
	};
	fetches.length = 0;

	await copy.click();
	await expect(page.getByTestId('queue-share-status')).toContainText(/copied/i);

	expect(await queueOrder(page), 'queue order must not change').toEqual(before.queue);
	expect(await currentQueueId(page), 'selection must not change').toBe(before.current);
	expect(await page.getByTestId('queue-position').textContent(), 'position must not change').toBe(
		before.position
	);
	expect(await page.getByTestId('now-playing').textContent(), 'now-playing must not change').toBe(
		before.nowPlaying
	);
	expect(await page.locator('#queue-filter-input').inputValue(), 'queue filter must not change').toBe(
		before.queueFilter
	);
	expect(await page.locator('#library-find').inputValue(), 'library find must not change').toBe(
		before.find
	);
	expect(await page.locator('#library-find-scope').inputValue(), 'find scope must not change').toBe(
		before.scope
	);
	expect(await page.locator('#library-order').inputValue(), 'order must not change').toBe(before.order);
	expect(
		await page
			.getByTestId('favorite-item')
			.evaluateAll((els) => els.map((el) => el.getAttribute('data-id'))),
		'favorites must not change'
	).toEqual(before.favorites);
	expect(
		await page.getByTestId('audio-player').evaluate((el: HTMLAudioElement) => el.playbackRate),
		'playback rate must not change'
	).toBe(before.rate);
	expect(await audioTime(page), 'copy must not seek').toBe(before.time);
	expect(await page.getByTestId('audio-player').count(), 'copy must not summon audio').toBe(
		before.audioCount
	);
	expect(await storageDump(page), 'copy must not persist anything').toEqual(before.storage);
	expect(await mediaCallCounts(page), 'copy must not play or pause').toEqual(before.media);
	expect(fetches, 'copy must perform no fetch').toEqual([]);
	expect(external, 'copy must not call external services').toEqual([]);
	expect(await execCommandCallCount(page), 'copy must not use execCommand').toBe(0);

	// Announcements stay scoped to the queue-share action.
	await expect(page.getByTestId('queue-share-status')).toContainText(/Pine Morning/);
	expect(
		await page.getByTestId('queue-announcement').textContent(),
		'queue-announcement must stay untouched'
	).toBe(before.queueAnnouncement);
	expect(
		await page.getByTestId('library-share-status').textContent(),
		'browse share status must stay untouched'
	).toBe(before.browseStatus);
	expect(
		await page.getByTestId('favorites-announcement').textContent(),
		'favorites announcement must stay untouched'
	).toBe(before.favoritesAnnouncement);
	const paused = await page
		.getByTestId('audio-player')
		.evaluate((el: HTMLAudioElement) => el.paused);
	expect(paused, 'sharing must never start audio').toBe(true);
});

test('[QUEUESHARE-12] only explicit activation touches the clipboard; success is concise, polite and focus-stable', async ({
	page
}) => {
	await openLibrary(page);
	expect(await clipboardCalls(page), 'no clipboard write on load').toEqual([]);
	expect(await execCommandCallCount(page), 'no execCommand on load').toBe(0);

	await seedQueue(page);
	expect(await clipboardCalls(page), 'no clipboard write on queueing').toEqual([]);

	// Selection, moves, seeks and timeupdates are not activation.
	await queueRow(page, 'qshare-pine')
		.getByRole('button', { name: 'Select Pine Morning', exact: true })
		.click();
	await expect(page.getByTestId('now-playing')).toContainText('Pine Morning');
	await queueRow(page, 'qshare-ember')
		.getByRole('button', { name: 'Move Ember Line up', exact: true })
		.click();
	await setAudioTime(page, 33);
	await page
		.getByTestId('audio-player')
		.evaluate((el) => el.dispatchEvent(new Event('timeupdate')));
	expect(await clipboardCalls(page), 'no clipboard write on select/move/seek').toEqual([]);

	const copy = await expectQueueCopyButton(page, 'QUEUESHARE-12', 'qshare-ember', 'Ember Line');
	await copy.click();
	await expect.poll(() => clipboardCalls(page)).toHaveLength(1);
	expect(await execCommandCallCount(page), 'activation must not use execCommand').toBe(0);

	const status = page.getByTestId('queue-share-status');
	await expect(status, 'success must announce a confirmation').toContainText(/copied/i);
	await expect(status, 'confirmation must name the copied track').toContainText(/Ember Line/);
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

	const focused = await page.evaluate(() => {
		const el = document.activeElement as HTMLElement | null;
		return {
			testid: el?.getAttribute('data-testid'),
			row: el?.closest('[data-testid="queue-item"]')?.getAttribute('data-track-id')
		};
	});
	expect(focused.testid, 'focus must stay on a copy action after success').toBe('queue-copy-link');
	expect(focused.row, 'focus must stay on the activating row').toBe('qshare-ember');
});
