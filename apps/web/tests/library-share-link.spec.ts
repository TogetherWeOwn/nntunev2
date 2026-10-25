import { expect, test, type Page } from '@playwright/test';

/**
 * Test-Author owned RED acceptance for LIBSHARE: copy a library track link
 * that reopens the track at its position. The Builder wires `/library` to
 * satisfy this file and must not edit it.
 *
 * LOCKED PAGE CONTRACT (new hooks on `/library`; existing rows, queue,
 * favorites, and order hooks are unchanged):
 * - Every `library-item` row carries a NATIVE <button>
 *   `library-copy-link` whose accessible name matches /copy link/i and names
 *   the row's track title (per-track naming, like the star toggles).
 *   Keyboard reachable with visible focus (:focus-visible +
 *   outline/box-shadow); Enter/Space activates the focused action.
 * - Activating a row's copy writes ONE fresh same-origin URL: origin +
 *   `/library?` + buildShareQuery(trackId, seconds) (only `track` + `t`;
 *   floor + encodeURIComponent semantics). The position is the CURRENT
 *   audio element's currentTime read live at activation when the row's
 *   track is the current track with usable media; otherwise t=0 (a
 *   non-current row is never stamped with another track's position, and a
 *   row is always copyable — even with an empty queue — because its track
 *   id is always known). Never clones the current search/hash and never
 *   uses a stream URL: token, session, grant, bearer, provider, fragment,
 *   and /test-audio material must not enter the result.
 * - `library-share-status`: one shared aria-live="polite" region (or
 *   role="status"). On a fulfilled write it announces a concise (<100
 *   chars) confirmation matching /copied/i and naming the copied track.
 *   On fallback/refusal it never claims success. Cleared on selection
 *   change and on fresh mount; late completion from a superseded action is
 *   ignored.
 * - `library-share-fallback`: one shared LABELED readonly <input> or
 *   <textarea> holding the share URL as its value (safe text, never
 *   innerHTML). Shown only when the Clipboard API is absent or rejects.
 *   Focusable and selectable; focus stays on the activating row button.
 * - `library-share-instructions`: actionable manual-copy instructions shown
 *   with the fallback (matches /copy/i, names a manual gesture:
 *   Ctrl/Cmd/right-click/etc.). Never claims success.
 * - Opening `/library?track=<id>&t=<seconds>` resolves via
 *   resolveShareTarget against the LOADED item ids (never partial):
 *   a hit selects the track and seeks the audio element to `t`
 *   (fractional kept, clock forms honored, clamped to duration when the
 *   duration is finite; unknown duration seeks exactly). No autoplay.
 * - `library-share-invalid`: role="alert" notice matching /invalid link/i,
 *   rendered at page level so it shows with or without loaded items. Shown
 *   for unknown/missing ids and unparsable/missing `t`; selection, queue,
 *   media position and storage are untouched (never partial: a bad `t`
 *   selects nothing, unlike the demo inbound fallback to 0).
 * - Only explicit activation may touch the clipboard: no writes on load,
 *   selection, seek, or timeupdate. No `document.execCommand` calls, no new
 *   external requests, no unsolicited clipboard attempts.
 * - Copy and open-link never play, pause, seek (except the open-link seek),
 *   change queue/order/rate, or persist identity (localStorage and
 *   sessionStorage unchanged apart from pre-existing load writes).
 * - Enabling carry-through: selection carries a loaded item's own
 *   `streamUrl` (when a usable non-blank string) into the queue so the
 *   existing `audio-player` renders; items without one keep today's
 *   select-only behavior (no audio element, copies at t=0).
 *
 * PROTECTED (Test-Author owned; Builder must not edit):
 * - `apps/web/tests/library-share-link.spec.ts` (this file)
 * - `apps/web/src/lib/library/shareLink.acceptance.test.ts`
 * BUILDER-OWNED implementation surface:
 * - `apps/web/src/routes/library/+page.svelte` (only)
 * - Existing share/seek suites stay unchanged.
 *
 * Run from the repository root (Node 24 / pnpm 10); builds + serves
 * locally, all APIs fulfilled in-test, no staging/prod access:
 *   CI=1 pnpm --dir apps/web test:e2e tests/library-share-link.spec.ts --workers=1
 * Unit companion (offline):
 *   pnpm --dir apps/web test src/lib/library/shareLink.acceptance.test.ts
 * Do not insert `--` after these pnpm scripts: this toolchain forwards it,
 * ending option parsing and selecting the entire suite. Ensure preview
 * port 4173 is free; never reuse another worktree's preview.
 *
 * RED NOW: `library-copy-link` does not exist, so every case below fails at
 * its button/selection assertion with a NEXT action. No import/compile
 * failure counts as RED: this file imports only `@playwright/test`.
 * Every RED assertion message is a compact JSON receipt:
 * {"case","expected","actual","status","error_code","next_action"}.
 */

const ITEMS = [
	{ id: 'share-coral', title: 'Coral Night', artist: 'Mira', streamUrl: '/test-audio/share-coral' },
	{ id: 'share-pine', title: 'Pine Morning', artist: 'North', streamUrl: '/test-audio/share-pine' },
	{ id: 'share-ember', title: 'Ember Line', streamUrl: '/test-audio/share-ember' }
];

const PLAIN_ITEMS = [
	{ id: 'plain-coral', title: 'Plain Coral', artist: 'Mira' },
	{ id: 'plain-pine', title: 'Plain Pine', artist: 'North' }
];

const COPY_LABEL = /copy link/i;

type ClipboardMode = 'fulfilled' | 'rejected' | 'missing' | 'delayed';

interface LibraryItem {
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
	options: { url?: string; items?: LibraryItem[]; clipboard?: ClipboardMode; duration?: number } = {}
) {
	const { url = '/library', items = ITEMS, clipboard = 'fulfilled', duration = null } = options;
	await page.route('**/api/library', (route) =>
		route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(items) })
	);
	await page.route('**/test-audio/**', () => {});
	await page.addInitScript(
		({ mode, mockedDuration }: { mode: ClipboardMode; mockedDuration: number | null }) => {
			const calls: string[] = [];
			const control: {
				calls: string[];
				deferred: { resolve: () => void; reject: (error: unknown) => void } | null;
			} = { calls, deferred: null };
			(window as unknown as { __libShareCalls: string[] }).__libShareCalls = calls;
			(window as unknown as { __libShareControl: unknown }).__libShareControl = control;
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
			(window as unknown as { __libShareExecCalls: unknown[][] }).__libShareExecCalls = execCalls;
			(document as unknown as { execCommand: (...args: unknown[]) => boolean }).execCommand = (
				...args
			) => {
				execCalls.push(args);
				return false;
			};
			const mediaCalls = { play: 0, pause: 0 };
			(window as unknown as { __libMediaCalls: { play: number; pause: number } }).__libMediaCalls =
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
		},
		{ mode: clipboard, mockedDuration: duration }
	);
	await page.goto(url);
	await expect(page.getByTestId('library-item')).toHaveCount(items.length);
}

function row(page: Page, id: string) {
	return page.locator(`[data-testid="library-item"][data-id="${id}"]`);
}

/**
 * Click the row's select control (the button whose accessible name STARTS
 * with the track title). Star ("Star X") and copy ("Copy link X") buttons
 * share the title substring, so a plain name match would be ambiguous once
 * the Builder adds the copy action; the `^Title` anchor keeps selection
 * clicks on the select control in both RED and GREEN trees.
 */
async function selectRow(page: Page, id: string, title: string) {
	const escaped = title.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
	await row(page, id)
		.getByRole('button', { name: new RegExp(`^${escaped}( — |$)`) })
		.click();
}

async function clipboardCalls(page: Page): Promise<string[]> {
	return page.evaluate(
		() => (window as unknown as { __libShareCalls: string[] }).__libShareCalls ?? []
	);
}

async function execCommandCallCount(page: Page): Promise<number> {
	return page.evaluate(
		() => ((window as unknown as { __libShareExecCalls: unknown[] }).__libShareExecCalls ?? []).length
	);
}

async function mediaCallCounts(page: Page): Promise<{ play: number; pause: number }> {
	return page.evaluate(
		() =>
			(window as unknown as { __libMediaCalls: { play: number; pause: number } }).__libMediaCalls ?? {
				play: 0,
				pause: 0
			}
	);
}

async function setAudioTime(page: Page, seconds: number) {
	await page
		.getByTestId('audio-player')
		.evaluate((el, value) => {
			(el as HTMLAudioElement).currentTime = value;
		}, seconds);
}

async function mockAudioTime(page: Page, value: number) {
	await page.getByTestId('audio-player').evaluate((el, mocked) => {
		Object.defineProperty(el, 'currentTime', { value: mocked, configurable: true });
	}, value);
}

async function audioTime(page: Page) {
	return page.getByTestId('audio-player').evaluate((el: HTMLAudioElement) => el.currentTime);
}

async function audioCount(page: Page) {
	return page.getByTestId('audio-player').count();
}

async function currentQueueId(page: Page): Promise<string | null> {
	return page
		.locator('[data-testid="queue-item"][aria-current="true"]')
		.evaluateAll((els) => els.map((el) => el.getAttribute('data-track-id')))
		.then((ids) => ids[0] ?? null);
}

async function queueOrder(page: Page) {
	return page
		.getByTestId('queue-item')
		.evaluateAll((els) => els.map((el) => el.getAttribute('data-track-id')));
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
				__libShareControl: { deferred: { resolve: () => void } | null };
			}
		).__libShareControl;
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

async function expectRowCopyButton(page: Page, caseId: string, id: string, title: string) {
	const copy = row(page, id).getByTestId('library-copy-link');
	await expect(
		copy,
		note(
			caseId,
			`a labeled native Copy button in row ${id}`,
			'missing',
			'row_copy_missing',
			`Builder adds <button data-testid="library-copy-link">Copy link ${title}</button> to every library row.`
		)
	).toBeVisible();
	await expect(copy, `${caseId}: expected a native <button>.`).toHaveJSProperty('tagName', 'BUTTON');
	await expect(copy, `${caseId}: expected accessible name matching /copy link/i.`).toHaveAccessibleName(
		COPY_LABEL
	);
	await expect(
		copy,
		`${caseId}: expected the accessible name to carry the track title "${title}".`
	).toHaveAccessibleName(new RegExp(title.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'), 'i'));
	return copy;
}

test('[LIBSHARE-01] rows expose labeled native copy actions, keyboard-focusable with visible focus', async ({
	page
}) => {
	await openLibrary(page);
	const copy = await expectRowCopyButton(page, 'LIBSHARE-01', 'share-coral', 'Coral Night');

	// Keyboard reachability + visible focus (modelled on the demo copy case).
	let found = false;
	for (let tab = 0; tab < 80 && !found; tab += 1) {
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
		if (focused?.testid === 'library-copy-link') {
			found = true;
			expect(focused.focusVisible, 'library-copy-link must match :focus-visible').toBe(true);
			const visible =
				focused.outlineWidth !== '0px' && focused.outlineWidth !== '0'
					? true
					: focused.boxShadow !== 'none';
			expect(visible, 'library-copy-link must show a visible focus indicator').toBe(true);
		}
	}
	expect(found, 'LIBSHARE-01: library-copy-link must be reachable by Tab').toBe(true);

	// Keyboard operation: Enter activates the copy (empty queue: t=0).
	await copy.focus();
	await page.keyboard.press('Enter');
	await expect
		.poll(() => clipboardCalls(page), 'Enter on library-copy-link must write the clipboard')
		.toHaveLength(1);
	const [written] = await clipboardCalls(page);
	expectShareUrl(written ?? '', { track: 'share-coral', t: 0 }, page.url());
});

test('[LIBSHARE-02] current-row copy writes a fresh same-origin URL with floored live seconds', async ({
	page
}) => {
	await openLibrary(page);
	await selectRow(page, 'share-coral', 'Coral Night');
	await expect(page.getByTestId('audio-player')).toHaveCount(1);
	const copy = await expectRowCopyButton(page, 'LIBSHARE-02', 'share-coral', 'Coral Night');

	await setAudioTime(page, 95.9);
	expect(await clipboardCalls(page), 'no clipboard write before activation').toEqual([]);

	await copy.click();
	await expect.poll(() => clipboardCalls(page)).toHaveLength(1);
	const [written] = await clipboardCalls(page);
	expectShareUrl(written ?? '', { track: 'share-coral', t: 95 }, page.url());

	const status = page.getByTestId('library-share-status');
	await expect(status, 'success must announce a confirmation').toContainText(/copied/i);
	await expect(status, 'confirmation must name the copied track').toContainText(/Coral Night/);
});

test('[LIBSHARE-03] non-current rows copy t=0 and change nothing', async ({ page }) => {
	await openLibrary(page);
	await selectRow(page, 'share-coral', 'Coral Night');
	await expect(page.getByTestId('audio-player')).toHaveCount(1);
	await setAudioTime(page, 42);
	const copy = await expectRowCopyButton(page, 'LIBSHARE-03', 'share-pine', 'Pine Morning');

	// A row is never stamped with another track's position.
	await copy.click();
	await expect.poll(() => clipboardCalls(page)).toHaveLength(1);
	const [written] = await clipboardCalls(page);
	expectShareUrl(written ?? '', { track: 'share-pine', t: 0 }, page.url());

	// Copying a row selects nothing: the queue still holds only coral, current.
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	expect(await currentQueueId(page)).toBe('share-coral');
	await expect(page.getByTestId('now-playing')).toContainText('Coral Night');
	expect(await audioTime(page), 'copy must not seek').toBe(42);
});

test('[LIBSHARE-04] copied URL carries only track/t: no search, hash, grant, or stream material', async ({
	page
}) => {
	// Synthetic token-like fixture values only; none may enter the result.
	await openLibrary(page, {
		url: '/library?track=share-pine&t=10&token=SYNTHETIC_TOKEN_123&session=SYNTHETIC_SESSION_abc&grant=signed-grant-xyz&provider=acme#/frag?bearer=SYNTHETIC_BEARER'
	});
	await expect(page.getByTestId('now-playing')).toContainText('Pine Morning');
	const copy = await expectRowCopyButton(page, 'LIBSHARE-04', 'share-pine', 'Pine Morning');

	await setAudioTime(page, 42.7);
	await copy.click();
	await expect.poll(() => clipboardCalls(page)).toHaveLength(1);
	const [written] = await clipboardCalls(page);
	expectShareUrl(written ?? '', { track: 'share-pine', t: 42 }, page.url());

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
		'provider'
	]) {
		expect(written ?? '', `result must not contain fixture material "${leaked}"`).not.toContain(
			leaked
		);
	}
	expect(written ?? '', 'result must not contain a fragment').not.toContain('#');
});

test('[LIBSHARE-05] zero time copies t=0', async ({ page }) => {
	await openLibrary(page);
	await selectRow(page, 'share-ember', 'Ember Line');
	await expect(page.getByTestId('audio-player')).toHaveCount(1);
	const copy = await expectRowCopyButton(page, 'LIBSHARE-05', 'share-ember', 'Ember Line');

	await setAudioTime(page, 0);
	await expect(copy, 'zero is a valid share time; the action stays enabled').toBeEnabled();
	await copy.click();
	await expect.poll(() => clipboardCalls(page)).toHaveLength(1);
	const [written] = await clipboardCalls(page);
	expectShareUrl(written ?? '', { track: 'share-ember', t: 0 }, page.url());
	await expect(page.getByTestId('library-share-status')).toContainText(/copied/i);
});

test('[LIBSHARE-06] unusable media disables or refuses the current row without clipboard access', async ({
	page
}) => {
	await openLibrary(page);
	await selectRow(page, 'share-coral', 'Coral Night');
	await expect(page.getByTestId('audio-player')).toHaveCount(1);
	const copy = await expectRowCopyButton(page, 'LIBSHARE-06', 'share-coral', 'Coral Night');

	for (const invalid of [Number.NaN, Number.POSITIVE_INFINITY, -1]) {
		await mockAudioTime(page, invalid);
		const before = (await clipboardCalls(page)).length;
		if (await copy.isDisabled()) continue;
		// Enabled but invalid: activation must refuse (no write, no success claim).
		await copy.click();
		await page.waitForTimeout(200);
		expect(
			await clipboardCalls(page),
			`LIBSHARE-06: time=${String(invalid)} must not write the clipboard`
		).toHaveLength(before);
		await expect(page.getByTestId('library-share-status')).not.toContainText(/copied|success/i);
	}
	expect(await execCommandCallCount(page), 'refusal must not use execCommand').toBe(0);

	// Non-current rows never read media: they stay enabled and copy t=0.
	// The refusing loop above wrote nothing, so this is the first write.
	const other = await expectRowCopyButton(page, 'LIBSHARE-06', 'share-pine', 'Pine Morning');
	await expect(other, 'non-current rows stay enabled with unusable media').toBeEnabled();
	await other.click();
	await expect
		.poll(() => clipboardCalls(page), 'non-current row must still copy')
		.toHaveLength(1);
	const [written] = await clipboardCalls(page);
	expectShareUrl(written ?? '', { track: 'share-pine', t: 0 }, page.url());
});

test('[LIBSHARE-07] empty queue: row copy works at t=0', async ({ page }) => {
	await openLibrary(page);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	const copy = await expectRowCopyButton(page, 'LIBSHARE-07', 'share-coral', 'Coral Night');

	// The row id is always known, so an empty queue never disables row copy.
	await expect(copy, 'row copy stays enabled with an empty queue').toBeEnabled();
	await copy.click();
	await expect.poll(() => clipboardCalls(page)).toHaveLength(1);
	const [written] = await clipboardCalls(page);
	expectShareUrl(written ?? '', { track: 'share-coral', t: 0 }, page.url());
	await expect(page.getByTestId('library-share-status')).toContainText(/copied/i);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
});

test('[LIBSHARE-08] only explicit activation touches the clipboard; no execCommand or external calls', async ({
	page
}) => {
	const external: string[] = [];
	page.on('request', (request) => {
		const host = new URL(request.url()).hostname;
		if (host !== 'localhost' && host !== '127.0.0.1') external.push(request.url());
	});
	await openLibrary(page);
	expect(await clipboardCalls(page), 'no clipboard write on load').toEqual([]);
	expect(await execCommandCallCount(page), 'no execCommand on load').toBe(0);

	// Selection, seeks, and timeupdates are not activation.
	await selectRow(page, 'share-coral', 'Coral Night');
	await expect(page.getByTestId('audio-player')).toHaveCount(1);
	await setAudioTime(page, 33);
	await page
		.getByTestId('audio-player')
		.evaluate((el) => el.dispatchEvent(new Event('timeupdate')));
	expect(await clipboardCalls(page), 'no clipboard write on select/seek').toEqual([]);

	const copy = await expectRowCopyButton(page, 'LIBSHARE-08', 'share-coral', 'Coral Night');
	await copy.click();
	await expect.poll(() => clipboardCalls(page)).toHaveLength(1);
	expect(await execCommandCallCount(page), 'activation must not use execCommand').toBe(0);
	expect(external, 'copy must not call external services').toEqual([]);
});

test('[LIBSHARE-09] success announces a concise polite confirmation and keeps focus stable', async ({
	page
}) => {
	await openLibrary(page);
	await selectRow(page, 'share-coral', 'Coral Night');
	await expect(page.getByTestId('audio-player')).toHaveCount(1);
	const copy = await expectRowCopyButton(page, 'LIBSHARE-09', 'share-coral', 'Coral Night');

	await setAudioTime(page, 30.4);
	await copy.click();
	const status = page.getByTestId('library-share-status');
	await expect(status, 'success must announce a confirmation').toContainText(/copied/i);
	await expect(status, 'confirmation must name the copied track').toContainText(/Coral Night/);
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
			row: el?.closest('[data-testid="library-item"]')?.getAttribute('data-id')
		};
	});
	expect(focused.testid, 'focus must stay on a copy action after success').toBe(
		'library-copy-link'
	);
	expect(focused.row, 'focus must stay on the activating row').toBe('share-coral');
});

test('[LIBSHARE-10] missing Clipboard API shows a labeled readonly fallback with instructions', async ({
	page
}) => {
	await openLibrary(page, { clipboard: 'missing' });
	await selectRow(page, 'share-coral', 'Coral Night');
	await expect(page.getByTestId('audio-player')).toHaveCount(1);
	const copy = await expectRowCopyButton(page, 'LIBSHARE-10', 'share-coral', 'Coral Night');

	await setAudioTime(page, 25.3);
	await copy.click();
	const fallback = page.getByTestId('library-share-fallback');
	await expect(fallback, 'fallback URL must appear when clipboard is absent').toBeVisible();
	const tag = await fallback.evaluate((el) => el.tagName);
	expect(tag === 'INPUT' || tag === 'TEXTAREA', `fallback must be input/textarea (actual ${tag})`).toBe(
		true
	);
	await expect(fallback, 'fallback must be labeled').toHaveAccessibleName(/.+/);
	const readonly = await fallback.evaluate((el) => (el as HTMLInputElement).readOnly);
	expect(readonly, 'fallback must be readonly').toBe(true);
	const value = await fallback.inputValue();
	expectShareUrl(value, { track: 'share-coral', t: 25 }, page.url());

	const instructions = page.getByTestId('library-share-instructions');
	await expect(instructions, 'fallback must explain the manual copy').toBeVisible();
	await expect(instructions).toContainText(/copy/i);
	await expect(instructions, 'instructions must name a manual gesture').toContainText(
		/ctrl|cmd|⌘|right-click|long-press|keyboard/i
	);
	await expect(instructions, 'fallback must not claim success').not.toContainText(/copied|success/i);
	await expect(page.getByTestId('library-share-status')).not.toContainText(/copied|success/i);

	// Focus stays put, but the fallback is focusable and fully selectable.
	const focused = await page.evaluate(() => {
		const el = document.activeElement as HTMLElement | null;
		return {
			testid: el?.getAttribute('data-testid'),
			row: el?.closest('[data-testid="library-item"]')?.getAttribute('data-id')
		};
	});
	expect(focused.testid, 'focus must stay on a copy action when the fallback appears').toBe(
		'library-copy-link'
	);
	expect(focused.row, 'focus must stay on the activating row').toBe('share-coral');
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

test('[LIBSHARE-11] rejected clipboard write falls back without claiming success', async ({ page }) => {
	await openLibrary(page, { clipboard: 'rejected' });
	await selectRow(page, 'share-coral', 'Coral Night');
	await expect(page.getByTestId('audio-player')).toHaveCount(1);
	const copy = await expectRowCopyButton(page, 'LIBSHARE-11', 'share-coral', 'Coral Night');

	await setAudioTime(page, 61.2);
	await copy.click();
	const fallback = page.getByTestId('library-share-fallback');
	await expect(fallback, 'fallback URL must appear when the write rejects').toBeVisible();
	expectShareUrl(await fallback.inputValue(), { track: 'share-coral', t: 61 }, page.url());
	await expect(page.getByTestId('library-share-instructions')).toContainText(/copy/i);
	await expect(page.getByTestId('library-share-instructions')).not.toContainText(/copied|success/i);
	await expect(page.getByTestId('library-share-status')).not.toContainText(/copied|success/i);
	expect(await execCommandCallCount(page), 'rejection must not use execCommand').toBe(0);
	expect(await queueOrder(page), 'rejection must not change the queue').toEqual(['share-coral']);
});

test('[LIBSHARE-12] fallback renders the URL as safe text: no HTML injection from query or hash', async ({
	page
}) => {
	await openLibrary(page, {
		url: '/library?track=share-coral&t=5&x=%3Cimg%20src%3Dx%20onerror%3Dalert(1)%3E#%3Cscript%3Ealert(2)%3C%2Fscript%3E',
		clipboard: 'missing'
	});
	await expect(page.getByTestId('now-playing')).toContainText('Coral Night');
	const copy = await expectRowCopyButton(page, 'LIBSHARE-12', 'share-coral', 'Coral Night');

	await setAudioTime(page, 10);
	await copy.click();
	const fallback = page.getByTestId('library-share-fallback');
	await expect(fallback).toBeVisible();
	const value = await fallback.inputValue();
	expectShareUrl(value, { track: 'share-coral', t: 10 }, page.url());
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
	await expect(page.getByTestId('library-share-instructions')).not.toContainText(/<img|<script/i);
});

test('[LIBSHARE-13] selection change and fresh mount clear stale success state', async ({ page }) => {
	await openLibrary(page);
	const copy = await expectRowCopyButton(page, 'LIBSHARE-13', 'share-coral', 'Coral Night');

	await copy.click();
	await expect(page.getByTestId('library-share-status')).toContainText(/copied/i);

	// A selection change clears the fresh success.
	await selectRow(page, 'share-pine', 'Pine Morning');
	await expect(
		page.getByTestId('library-share-status'),
		'success must clear on selection change'
	).not.toContainText(/copied/i);

	// A fresh mount never resurrects status or fallback.
	const pineCopy = await expectRowCopyButton(page, 'LIBSHARE-13', 'share-pine', 'Pine Morning');
	await pineCopy.click();
	await expect(page.getByTestId('library-share-status')).toContainText(/copied/i);
	await page.reload();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(
		page.getByTestId('library-share-status'),
		'fresh mount must not resurrect the previous success'
	).not.toContainText(/copied/i);
	expect(
		await page.getByTestId('library-share-fallback').count(),
		'fresh mount must not resurrect the fallback'
	).toBe(0);
});

test('[LIBSHARE-14] selection change supersedes a pending write: late completion stays ignored', async ({
	page
}) => {
	await openLibrary(page, { clipboard: 'delayed' });
	const copy = await expectRowCopyButton(page, 'LIBSHARE-14', 'share-coral', 'Coral Night');

	// A valid activation with a pending clipboard write: no success yet.
	await copy.click();
	await expect(page.getByTestId('library-share-status')).not.toContainText(/copied/i);

	// Supersede the pending write, then let it complete late: it must be ignored.
	await selectRow(page, 'share-pine', 'Pine Morning');
	await resolveDelayedWrite(page);
	await page.waitForTimeout(200);
	await expect(
		page.getByTestId('library-share-status'),
		'late completion from a superseded action must be ignored'
	).not.toContainText(/copied/i);
	expect(
		await page.getByTestId('library-share-fallback').count(),
		'no stale fallback from the superseded action'
	).toBe(0);

	// The next action on the new selection still works.
	const pineCopy = await expectRowCopyButton(page, 'LIBSHARE-14', 'share-pine', 'Pine Morning');
	await pineCopy.click();
	await resolveDelayedWrite(page);
	await expect(page.getByTestId('library-share-status')).toContainText(/copied/i);
	const calls = await clipboardCalls(page);
	expect(calls, 'expected one write per activation').toHaveLength(2);
	expectShareUrl(calls[1] ?? '', { track: 'share-pine', t: 0 }, page.url());
});

test('[LIBSHARE-15] copy never plays, pauses, seeks, changes queue/order/selection/rate, or persists identity', async ({
	page
}) => {
	await openLibrary(page);
	await selectRow(page, 'share-coral', 'Coral Night');
	await expect(page.getByTestId('audio-player')).toHaveCount(1);
	const copy = await expectRowCopyButton(page, 'LIBSHARE-15', 'share-coral', 'Coral Night');

	await setAudioTime(page, 50);
	const before = {
		queue: await queueOrder(page),
		current: await currentQueueId(page),
		order: await page.locator('#library-order').inputValue(),
		rate: await page.getByTestId('audio-player').evaluate((el: HTMLAudioElement) => el.playbackRate),
		time: await audioTime(page),
		storage: await storageDump(page),
		media: await mediaCallCounts(page)
	};

	await copy.click();
	await expect(page.getByTestId('library-share-status')).toContainText(/copied/i);

	expect(await queueOrder(page), 'queue order must not change').toEqual(before.queue);
	expect(await currentQueueId(page), 'selection must not change').toBe(before.current);
	expect(await page.locator('#library-order').inputValue(), 'order must not change').toBe(
		before.order
	);
	expect(
		await page.getByTestId('audio-player').evaluate((el: HTMLAudioElement) => el.playbackRate),
		'playback rate must not change'
	).toBe(before.rate);
	expect(await audioTime(page), 'copy must not seek').toBe(before.time);
	expect(await storageDump(page), 'copy must not persist identity').toEqual(before.storage);
	expect(await mediaCallCounts(page), 'copy must not play or pause').toEqual(before.media);
});

test('[LIBSHARE-16] opening a valid link selects the track and seeks without autoplay', async ({
	page
}) => {
	await openLibrary(page, { url: '/library?track=share-pine&t=77' });

	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	expect(await currentQueueId(page)).toBe('share-pine');
	await expect(page.getByTestId('now-playing')).toContainText('Pine Morning');
	await expect(page.getByTestId('audio-player')).toHaveCount(1);
	await expect
		.poll(() => audioTime(page), 'open link must seek the audio element to t')
		.toBe(77);
	expect(
		await page.getByTestId('library-share-invalid').count(),
		'a valid link must not show the invalid notice'
	).toBe(0);
	expect(await mediaCallCounts(page), 'opening a link must not autoplay').toEqual({
		play: 0,
		pause: 0
	});
	const paused = await page
		.getByTestId('audio-player')
		.evaluate((el: HTMLAudioElement) => el.paused);
	expect(paused, 'opening a share link must not autoplay').toBe(true);
});

test('[LIBSHARE-17] open-link seeks clock forms, keeps fractions, and clamps to a known duration', async ({
	page
}) => {
	// Clock form seeks the parsed seconds.
	await openLibrary(page, { url: '/library?track=share-pine&t=1:30' });
	await expect(page.getByTestId('now-playing')).toContainText('Pine Morning');
	await expect.poll(() => audioTime(page), 'clock-form t must seek 90').toBe(90);

	// Fractional input keeps its fraction (demo inbound parity).
	await page.goto('/library?track=share-ember&t=77.8');
	await expect(page.getByTestId('now-playing')).toContainText('Ember Line');
	await expect
		.poll(() => audioTime(page), 'fractional t must keep its fraction')
		.toBeCloseTo(77.8, 5);
});

test('[LIBSHARE-18] open-link clamps t to a known finite duration', async ({
	page
}) => {
	// Known finite duration: t beyond it clamps.
	await openLibrary(page, { url: '/library?track=share-pine&t=90', duration: 60 });
	await expect(page.getByTestId('now-playing')).toContainText('Pine Morning');
	await expect
		.poll(() => audioTime(page), 't beyond a known duration must clamp')
		.toBe(60);
});

test('[LIBSHARE-19] invalid links show the invalid notice and change nothing', async ({ page }) => {
	const invalidUrls = [
		'/library?track=no-such-track&t=90',
		'/library?t=90',
		'/library?track=share-coral&t=soon',
		'/library?track=share-coral',
		'/library?track=share-coral&t=-3'
	];
	for (const invalidUrl of invalidUrls) {
		await openLibrary(page, { url: invalidUrl });
		const notice = page.getByTestId('library-share-invalid');
		await expect(
			notice,
			note(
				'LIBSHARE-19',
				`invalid-link notice for ${invalidUrl}`,
				'missing',
				'invalid_notice_missing',
				'Builder renders <p data-testid="library-share-invalid" role="alert">Invalid link …</p> when ?track=/t= fail to resolve.'
			)
		).toBeVisible();
		await expect(notice, 'invalid notice must match /invalid link/i').toContainText(/invalid link/i);
		const role = await notice.evaluate((el) => el.getAttribute('role'));
		expect(role, `invalid notice must use role=alert (actual role=${role})`).toBe('alert');

		// Never partial: nothing selected, no queue, no audio, no playback.
		await expect(page.getByTestId('queue-item')).toHaveCount(0);
		expect(await audioCount(page), 'invalid link must not summon audio').toBe(0);
		expect(await mediaCallCounts(page), 'invalid link must not autoplay').toEqual({
			play: 0,
			pause: 0
		});
		// The loaded list itself is untouched.
		await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	}
});

test('[LIBSHARE-20] links resolve only against loaded items; empty library stays invalid-safe', async ({
	page
}) => {
	// Foreign ids — demo catalogue, padded, blank — never resolve here.
	for (const foreign of ['demo-track', 'second-signal', ' share-coral ', '']) {
		await openLibrary(page, { url: `/library?track=${encodeURIComponent(foreign)}&t=10` });
		await expect(
			page.getByTestId('library-share-invalid'),
			note(
				'LIBSHARE-20',
				`invalid-link notice for foreign id "${foreign}"`,
				'missing',
				'foreign_id_resolved',
				'Builder resolves ?track= only against currently loaded item ids (never the demo catalogue, never trimmed).'
			)
		).toBeVisible();
		await expect(page.getByTestId('queue-item')).toHaveCount(0);
		expect(await audioCount(page)).toBe(0);
	}

	// Empty library: the link cannot resolve, the empty state stays intact.
	await openLibrary(page, { url: '/library?track=share-coral&t=10', items: [] });
	await expect(page.getByTestId('library-item')).toHaveCount(0);
	await expect(
		page.getByTestId('library-share-invalid'),
		'empty library must still show the invalid notice (page-level placement)'
	).toBeVisible();
	await expect(page.getByRole('status')).toContainText(/library is empty/i);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	expect(await audioCount(page)).toBe(0);
});

test('[LIBSHARE-21] items without stream URLs keep select-only behavior', async ({ page }) => {
	await openLibrary(page, { items: PLAIN_ITEMS });
	await selectRow(page, 'plain-coral', 'Plain Coral');
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	expect(
		await audioCount(page),
		'selecting a stream-less item must not summon audio (today behavior)'
	).toBe(0);

	// Copies still work, at t=0: no media position exists.
	const copy = await expectRowCopyButton(page, 'LIBSHARE-21', 'plain-coral', 'Plain Coral');
	await copy.click();
	await expect.poll(() => clipboardCalls(page)).toHaveLength(1);
	const [written] = await clipboardCalls(page);
	expectShareUrl(written ?? '', { track: 'plain-coral', t: 0 }, page.url());

	// Opening a link selects without audio, without the invalid notice, without crashing.
	await page.goto('/library?track=plain-pine&t=45');
	await expect(page.getByTestId('library-item')).toHaveCount(PLAIN_ITEMS.length);
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	expect(await currentQueueId(page)).toBe('plain-pine');
	await expect(page.getByTestId('now-playing')).toContainText('Plain Pine');
	expect(await audioCount(page), 'stream-less open must not summon audio').toBe(0);
	expect(
		await page.getByTestId('library-share-invalid').count(),
		'a resolving link must not show the invalid notice'
	).toBe(0);
	expect(await mediaCallCounts(page), 'stream-less open must not autoplay').toEqual({
		play: 0,
		pause: 0
	});
});

test('[LIBSHARE-22] successive copies read live state at activation (no stale snapshot)', async ({
	page
}) => {
	await openLibrary(page);
	await selectRow(page, 'share-coral', 'Coral Night');
	await expect(page.getByTestId('audio-player')).toHaveCount(1);
	const coralCopy = await expectRowCopyButton(page, 'LIBSHARE-22', 'share-coral', 'Coral Night');

	await setAudioTime(page, 10);
	await coralCopy.click();
	await expect.poll(() => clipboardCalls(page)).toHaveLength(1);

	// Change only the media time: the next copy must read it fresh.
	await setAudioTime(page, 50.2);
	await coralCopy.click();
	await expect.poll(() => clipboardCalls(page)).toHaveLength(2);

	await selectRow(page, 'share-pine', 'Pine Morning');
	// Library selection appends without stealing current, so advance
	// explicitly before sampling pine's position.
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Pine Morning');
	expect(await currentQueueId(page)).toBe('share-pine');
	await setAudioTime(page, 5.9);
	const pineCopy = await expectRowCopyButton(page, 'LIBSHARE-22', 'share-pine', 'Pine Morning');
	await pineCopy.click();
	await expect.poll(() => clipboardCalls(page)).toHaveLength(3);

	// Coral is no longer current: its next copy stamps t=0, not stale time.
	await coralCopy.click();
	await expect.poll(() => clipboardCalls(page)).toHaveLength(4);

	const calls = await clipboardCalls(page);
	expectShareUrl(calls[0] ?? '', { track: 'share-coral', t: 10 }, page.url());
	expectShareUrl(calls[1] ?? '', { track: 'share-coral', t: 50 }, page.url());
	expectShareUrl(calls[2] ?? '', { track: 'share-pine', t: 5 }, page.url());
	expectShareUrl(calls[3] ?? '', { track: 'share-coral', t: 0 }, page.url());
});
