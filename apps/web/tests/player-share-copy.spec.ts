import { expect, test, type Page } from '@playwright/test';

/**
 * Test-Author owned RED acceptance for COPY-TIME: copy a demo-track link
 * at the current playback position. The Builder wires `/demo/player` to
 * satisfy this file and must not edit it.
 *
 * LOCKED PAGE CONTRACT (new hooks on `/demo/player`; existing comforts,
 * queue, and inbound-link hooks are unchanged):
 * - `share-copy`: a NATIVE <button> labeled "Copy link at current time"
 *   (accessible name matches /copy link at current time/i). Keyboard
 *   reachable with visible focus (:focus-visible + outline/box-shadow).
 *   Disabled when there is no current recognized demo track (empty
 *   queue); for non-finite/negative media time it is either disabled or
 *   refuses on activation (no clipboard write, no success claim).
 * - `share-copy-status`: aria-live="polite" region (or role="status").
 *   On a fulfilled clipboard write it announces a concise (<100 chars)
 *   confirmation matching /copied/i. On fallback/refusal it never claims
 *   success (no /copied|success/i). Cleared on track/source change and on
 *   fresh mount; late completion from a superseded action is ignored.
 * - `share-copy-fallback`: a LABELED readonly <input> or <textarea>
 *   holding the share URL as its value (safe text, never innerHTML).
 *   Shown only when the Clipboard API is absent or rejects. Focusable
 *   and selectable (select() covers the full value). Focus stays on the
 *   copy button when the fallback appears (no focus steal).
 * - `share-copy-instructions`: actionable manual-copy instructions shown
 *   with the fallback (names a copy gesture: Ctrl/Cmd/right-click/etc.,
 *   matches /copy/i). Never claims success (no /copied|success/i).
 * - Activation reads the CURRENT matching audio element's currentTime at
 *   click time (no stale snapshot/cache) and copies a FRESH same-origin
 *   URL: origin + `/demo/player?` + buildShareQuery(trackId, seconds)
 *   (only `track` + `t`; floor + encodeURIComponent semantics). Never
 *   clones the current search/hash and never uses a stream URL: token,
 *   session, grant, bearer, provider, fragment, and /api/demo/audio
 *   material must not enter the result. Scope is the existing synthetic
 *   demo ids only (`demo-track`, `second-signal`, `third-light`).
 * - Only explicit activation may touch the clipboard: no writes on load,
 *   track change, seek, or timeupdate. No `document.execCommand` calls,
 *   no new external requests, no unsolicited clipboard attempts.
 * - Copy and fallback never play, pause, seek, change queue/order/rate,
 *   or persist identity (localStorage unchanged apart from pre-existing
 *   comfort keys written by their own controls).
 * - Opening a copied URL reuses inbound-link behavior: selects the demo
 *   track/time with NO autoplay (status never `playing` from the link).
 *
 * RED NOW: `share-copy` does not exist, so every case below fails at its
 * button assertion with a NEXT action. No import/compile failure counts
 * as RED: this file imports only `@playwright/test`.
 */

const DEMO_IDS = ['demo-track', 'second-signal', 'third-light'] as const;
const COPY_LABEL = /copy link at current time/i;

type ClipboardMode = 'fulfilled' | 'rejected' | 'missing' | 'delayed';

/** Keep media synthetic and pending; stub clipboard/media per mode. */
async function openPlayer(
	page: Page,
	options: { url?: string; clipboard?: ClipboardMode } = {}
) {
	const { url = '/demo/player', clipboard = 'fulfilled' } = options;
	await page.route('**/api/demo/audio**', () => {});
	await page.addInitScript((mode: ClipboardMode) => {
		const calls: string[] = [];
		const control: {
			calls: string[];
			deferred: { resolve: () => void; reject: (error: unknown) => void } | null;
		} = { calls, deferred: null };
		(window as unknown as { __shareCopyCalls: string[] }).__shareCopyCalls = calls;
		(window as unknown as { __shareCopyControl: unknown }).__shareCopyControl = control;
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
			...args
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
	await page.goto(url);
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
}

async function clipboardCalls(page: Page): Promise<string[]> {
	return page.evaluate(
		() => (window as unknown as { __shareCopyCalls: string[] }).__shareCopyCalls ?? []
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

async function mockCurrentTime(page: Page, value: number) {
	await page.getByTestId('demo-audio').evaluate((el, mocked) => {
		Object.defineProperty(el, 'currentTime', { value: mocked, configurable: true });
	}, value);
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
		const out: Record<string, string | null> = {};
		for (let i = 0; i < localStorage.length; i += 1) {
			const key = localStorage.key(i);
			if (key !== null) out[key] = localStorage.getItem(key);
		}
		return out;
	});
}

async function resolveDelayedWrite(page: Page) {
	await page.evaluate(() => {
		const control = (
			window as unknown as {
				__shareCopyControl: { deferred: { resolve: () => void } | null };
			}
		).__shareCopyControl;
		if (!control.deferred) throw new Error('no deferred clipboard write is pending');
		control.deferred.resolve();
	});
}

/** Fresh same-origin /demo/player URL with exactly track + t (floor semantics). */
function expectShareUrl(urlString: string, expected: { track: string; t: number }, pageUrl: string) {
	const url = new URL(urlString);
	const origin = new URL(pageUrl).origin;
	expect(url.origin, `expected same-origin ${origin} (actual ${url.origin})`).toBe(origin);
	expect(url.pathname, `expected pathname /demo/player (actual ${url.pathname})`).toBe(
		'/demo/player'
	);
	expect(url.hash, `expected no fragment (actual "${url.hash}")`).toBe('');
	expect(
		[...url.searchParams.keys()].sort(),
		`expected only track+t params (actual "${url.search}")`
	).toEqual(['t', 'track']);
	expect(url.searchParams.get('track'), 'expected track param to match').toBe(expected.track);
	expect(url.searchParams.get('t'), 'expected t param to match floored seconds').toBe(
		String(expected.t)
	);
	expect(urlString, 'must never use a stream URL').not.toContain('/api/demo/audio');
}

async function expectCopyButton(page: Page, caseId: string) {
	const copy = page.getByTestId('share-copy');
	await expect(
		copy,
		`${caseId}: expected a labeled native Copy button (actual: missing). NEXT: Builder adds <button data-testid="share-copy">Copy link at current time</button> to /demo/player.`
	).toBeVisible();
	await expect(copy, `${caseId}: expected a native <button>.`).toHaveJSProperty('tagName', 'BUTTON');
	await expect(copy, `${caseId}: expected accessible name "Copy link at current time".`).toHaveAccessibleName(
		COPY_LABEL
	);
	return copy;
}

test('[COPY-TIME-01] copy button is a labeled native action, keyboard-focusable with visible focus', async ({
	page
}) => {
	await openPlayer(page);
	const copy = await expectCopyButton(page, 'COPY-TIME-01');

	// Keyboard reachability + visible focus (modelled on the comforts keyboard case).
	let found = false;
	for (let tab = 0; tab < 60 && !found; tab += 1) {
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
		if (focused?.testid === 'share-copy') {
			found = true;
			expect(focused.focusVisible, 'share-copy must match :focus-visible').toBe(true);
			const visible =
				focused.outlineWidth !== '0px' && focused.outlineWidth !== '0'
					? true
					: focused.boxShadow !== 'none';
			expect(visible, 'share-copy must show a visible focus indicator').toBe(true);
		}
	}
	expect(found, 'COPY-TIME-01: share-copy must be reachable by Tab').toBe(true);

	// Keyboard operation: Enter activates the copy.
	await setCurrentTime(page, 12.9);
	await copy.focus();
	await page.keyboard.press('Enter');
	await expect
		.poll(() => clipboardCalls(page), 'Enter on share-copy must write the clipboard')
		.toHaveLength(1);
	const [written] = await clipboardCalls(page);
	expectShareUrl(written ?? '', { track: 'demo-track', t: 12 }, page.url());
});

test('[COPY-TIME-02] activation copies a fresh same-origin URL with floored seconds', async ({
	page
}) => {
	await openPlayer(page);
	const copy = await expectCopyButton(page, 'COPY-TIME-02');

	await setCurrentTime(page, 95.9);
	expect(await clipboardCalls(page), 'no clipboard write before activation').toEqual([]);

	await copy.click();
	await expect.poll(() => clipboardCalls(page)).toHaveLength(1);
	const [written] = await clipboardCalls(page);
	expectShareUrl(written ?? '', { track: 'demo-track', t: 95 }, page.url());

	await expect(page.getByTestId('share-copy-status')).toContainText(/copied/i);
});

test('[COPY-TIME-03] copied URL carries only track/t: no search, hash, grant, or stream material', async ({
	page
}) => {
	// Synthetic token-like fixture values only; none may enter the result.
	await openPlayer(page, {
		url: '/demo/player?track=second-signal&t=10&token=SYNTHETIC_TOKEN_123&session=SYNTHETIC_SESSION_abc&grant=signed-grant-xyz&provider=acme#/frag?bearer=SYNTHETIC_BEARER'
	});
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	const copy = await expectCopyButton(page, 'COPY-TIME-03');

	await setCurrentTime(page, 42.7);
	await copy.click();
	await expect.poll(() => clipboardCalls(page)).toHaveLength(1);
	const [written] = await clipboardCalls(page);
	expectShareUrl(written ?? '', { track: 'second-signal', t: 42 }, page.url());

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

test('[COPY-TIME-04] zero time copies t=0', async ({ page }) => {
	await openPlayer(page);
	const copy = await expectCopyButton(page, 'COPY-TIME-04');

	await setCurrentTime(page, 0);
	await expect(copy, 'zero is a valid share time; the action stays enabled').toBeEnabled();
	await copy.click();
	await expect.poll(() => clipboardCalls(page)).toHaveLength(1);
	const [written] = await clipboardCalls(page);
	expectShareUrl(written ?? '', { track: 'demo-track', t: 0 }, page.url());
	await expect(page.getByTestId('share-copy-status')).toContainText(/copied/i);
});

test('[COPY-TIME-05] non-finite or negative media time disables or refuses without clipboard access', async ({
	page
}) => {
	await openPlayer(page);
	const copy = await expectCopyButton(page, 'COPY-TIME-05');

	for (const invalid of [Number.NaN, Number.POSITIVE_INFINITY, -1]) {
		await mockCurrentTime(page, invalid);
		const before = (await clipboardCalls(page)).length;
		if (await copy.isDisabled()) continue;
		// Enabled but invalid: activation must refuse (no write, no success claim).
		await copy.click();
		await page.waitForTimeout(200);
		expect(
			await clipboardCalls(page),
			`COPY-TIME-05: time=${String(invalid)} must not write the clipboard`
		).toHaveLength(before);
		await expect(page.getByTestId('share-copy-status')).not.toContainText(/copied|success/i);
	}
	expect(await execCommandCallCount(page), 'refusal must not use execCommand').toBe(0);
});

test('[COPY-TIME-06] empty queue disables or refuses without clipboard access', async ({ page }) => {
	await openPlayer(page);
	await page.getByTestId('queue-clear').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
	const copy = await expectCopyButton(page, 'COPY-TIME-06');

	const before = (await clipboardCalls(page)).length;
	if (await copy.isDisabled()) {
		expect(await clipboardCalls(page), 'disabled action must not write').toHaveLength(before);
	} else {
		await copy.click();
		await page.waitForTimeout(200);
		expect(await clipboardCalls(page), 'refused action must not write').toHaveLength(before);
		await expect(page.getByTestId('share-copy-status')).not.toContainText(/copied|success/i);
	}
	expect(await execCommandCallCount(page), 'must not use execCommand').toBe(0);
});

test('[COPY-TIME-07] only explicit activation touches the clipboard; no execCommand or external calls', async ({
	page
}) => {
	const external: string[] = [];
	page.on('request', (request) => {
		const host = new URL(request.url()).hostname;
		if (host !== 'localhost' && host !== '127.0.0.1') external.push(request.url());
	});
	await openPlayer(page);
	expect(await clipboardCalls(page), 'no clipboard write on load').toEqual([]);
	expect(await execCommandCallCount(page), 'no execCommand on load').toBe(0);

	// Track changes, seeks, and timeupdates are not activation.
	await page.getByTestId('queue-select-second-signal').click();
	await setCurrentTime(page, 33);
	await page
		.getByTestId('demo-audio')
		.evaluate((el) => el.dispatchEvent(new Event('timeupdate')));
	expect(await clipboardCalls(page), 'no clipboard write on change/seek').toEqual([]);

	const copy = await expectCopyButton(page, 'COPY-TIME-07');
	await copy.click();
	await expect.poll(() => clipboardCalls(page)).toHaveLength(1);
	expect(await execCommandCallCount(page), 'activation must not use execCommand').toBe(0);
	expect(external, 'copy must not call external services').toEqual([]);
});

test('[COPY-TIME-08] success announces a concise polite confirmation and keeps focus stable', async ({
	page
}) => {
	await openPlayer(page);
	const copy = await expectCopyButton(page, 'COPY-TIME-08');

	await setCurrentTime(page, 30.4);
	await copy.click();
	const status = page.getByTestId('share-copy-status');
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
	expect(focused, 'focus must stay on the copy action after success').toBe('share-copy');
});

test('[COPY-TIME-09] missing Clipboard API shows a labeled readonly fallback with instructions', async ({
	page
}) => {
	await openPlayer(page, { clipboard: 'missing' });
	const copy = await expectCopyButton(page, 'COPY-TIME-09');

	await setCurrentTime(page, 25.3);
	await copy.click();
	const fallback = page.getByTestId('share-copy-fallback');
	await expect(fallback, 'fallback URL must appear when clipboard is absent').toBeVisible();
	const tag = await fallback.evaluate((el) => el.tagName);
	expect(tag === 'INPUT' || tag === 'TEXTAREA', `fallback must be input/textarea (actual ${tag})`).toBe(
		true
	);
	await expect(fallback, 'fallback must be labeled').toHaveAccessibleName(/.+/);
	const readonly = await fallback.evaluate((el) => (el as HTMLInputElement).readOnly);
	expect(readonly, 'fallback must be readonly').toBe(true);
	const value = await fallback.inputValue();
	expectShareUrl(value, { track: 'demo-track', t: 25 }, page.url());

	const instructions = page.getByTestId('share-copy-instructions');
	await expect(instructions, 'fallback must explain the manual copy').toBeVisible();
	await expect(instructions).toContainText(/copy/i);
	await expect(instructions, 'instructions must name a manual gesture').toContainText(
		/ctrl|cmd|⌘|right-click|long-press|keyboard/i
	);
	await expect(instructions, 'fallback must not claim success').not.toContainText(/copied|success/i);
	await expect(page.getByTestId('share-copy-status')).not.toContainText(/copied|success/i);

	// Focus stays put, but the fallback is focusable and fully selectable.
	const focused = await page.evaluate(
		() => (document.activeElement as HTMLElement | null)?.getAttribute('data-testid')
	);
	expect(focused, 'focus must stay on the copy action when the fallback appears').toBe('share-copy');
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

test('[COPY-TIME-10] rejected clipboard write falls back without claiming success', async ({
	page
}) => {
	await openPlayer(page, { clipboard: 'rejected' });
	const copy = await expectCopyButton(page, 'COPY-TIME-10');

	await setCurrentTime(page, 61.2);
	await copy.click();
	const fallback = page.getByTestId('share-copy-fallback');
	await expect(fallback, 'fallback URL must appear when the write rejects').toBeVisible();
	expectShareUrl(await fallback.inputValue(), { track: 'demo-track', t: 61 }, page.url());
	await expect(page.getByTestId('share-copy-instructions')).toContainText(/copy/i);
	await expect(page.getByTestId('share-copy-instructions')).not.toContainText(/copied|success/i);
	await expect(page.getByTestId('share-copy-status')).not.toContainText(/copied|success/i);
	expect(await execCommandCallCount(page), 'rejection must not use execCommand').toBe(0);
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
});

test('[COPY-TIME-11] fallback renders the URL as safe text: no HTML injection from query or hash', async ({
	page
}) => {
	await openPlayer(page, {
		url: '/demo/player?track=demo-track&t=5&x=%3Cimg%20src%3Dx%20onerror%3Dalert(1)%3E#%3Cscript%3Ealert(2)%3C%2Fscript%3E',
		clipboard: 'missing'
	});
	const copy = await expectCopyButton(page, 'COPY-TIME-11');

	await setCurrentTime(page, 10);
	await copy.click();
	const fallback = page.getByTestId('share-copy-fallback');
	await expect(fallback).toBeVisible();
	const value = await fallback.inputValue();
	expectShareUrl(value, { track: 'demo-track', t: 10 }, page.url());
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
	await expect(page.getByTestId('share-copy-instructions')).not.toContainText(/<img|<script/i);
});

test('[COPY-TIME-12] track change clears stale state and supersedes a late completion', async ({
	page
}) => {
	await openPlayer(page, { clipboard: 'delayed' });
	const copy = await expectCopyButton(page, 'COPY-TIME-12');

	await setCurrentTime(page, 10);
	await copy.click();
	await expect(page.getByTestId('share-copy-status')).not.toContainText(/copied/i);

	// Supersede the pending write, then let it complete late: it must be ignored.
	await page.getByTestId('queue-select-second-signal').click();
	await resolveDelayedWrite(page);
	await page.waitForTimeout(200);
	await expect(
		page.getByTestId('share-copy-status'),
		'late completion from a superseded action must be ignored'
	).not.toContainText(/copied/i);
	expect(
		await page.getByTestId('share-copy-fallback').count(),
		'no stale fallback from the superseded action'
	).toBe(0);

	// The next action on the new source still works.
	await setCurrentTime(page, 20.6);
	await copy.click();
	await resolveDelayedWrite(page);
	await expect(page.getByTestId('share-copy-status')).toContainText(/copied/i);
	const calls = await clipboardCalls(page);
	expect(calls, 'expected one write per activation').toHaveLength(2);
	expectShareUrl(calls[1] ?? '', { track: 'second-signal', t: 20 }, page.url());

	// A further track change clears the fresh success.
	await page.getByTestId('queue-select-third-light').click();
	await expect(
		page.getByTestId('share-copy-status'),
		'success must clear on track change'
	).not.toContainText(/copied/i);
});

test('[COPY-TIME-13] fresh mount clears stale success state (teardown)', async ({ page }) => {
	await openPlayer(page);
	const copy = await expectCopyButton(page, 'COPY-TIME-13');

	await setCurrentTime(page, 15);
	await copy.click();
	await expect(page.getByTestId('share-copy-status')).toContainText(/copied/i);

	await page.reload();
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(
		page.getByTestId('share-copy-status'),
		'fresh mount must not resurrect the previous success'
	).not.toContainText(/copied/i);
	expect(
		await page.getByTestId('share-copy-fallback').count(),
		'fresh mount must not resurrect the fallback'
	).toBe(0);
});

test('[COPY-TIME-14] copy never plays, pauses, seeks, changes queue/order/rate, or persists identity', async ({
	page
}) => {
	await openPlayer(page);
	const copy = await expectCopyButton(page, 'COPY-TIME-14');

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
	await expect(page.getByTestId('share-copy-status')).toContainText(/copied/i);

	expect(await page.getByTestId('player-status').textContent(), 'status must not change').toBe(
		before.status
	);
	expect(await queueOrder(page), 'queue order must not change').toEqual(before.queue);
	expect(
		await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => el.playbackRate),
		'playback rate must not change'
	).toBe(before.rate);
	expect(await currentTime(page), 'copy must not seek').toBe(before.time);
	expect(await storageSnapshot(page), 'copy must not persist identity').toEqual(before.storage);
	expect(await mediaCallCounts(page), 'copy must not play or pause').toEqual(before.media);
});

test('[COPY-TIME-15] opening the copied result selects the demo track/time without autoplay', async ({
	page
}) => {
	await openPlayer(page);
	await page.getByTestId('queue-select-second-signal').click();
	const copy = await expectCopyButton(page, 'COPY-TIME-15');

	await setCurrentTime(page, 77.8);
	await copy.click();
	await expect.poll(() => clipboardCalls(page)).toHaveLength(1);
	const [result] = await clipboardCalls(page);
	expectShareUrl(result ?? '', { track: 'second-signal', t: 77 }, page.url());

	await page.goto(result ?? '/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await expect.poll(() => currentTime(page)).toBe(77);
	await expect(page.getByTestId('player-status')).not.toHaveText('Status: playing');
	const paused = await page
		.getByTestId('demo-audio')
		.evaluate((el: HTMLAudioElement) => el.paused);
	expect(paused, 'opening a share link must not autoplay').toBe(true);
});

test('[COPY-TIME-16] successive copies read the live element at activation (no stale snapshot)', async ({
	page
}) => {
	await openPlayer(page);
	const copy = await expectCopyButton(page, 'COPY-TIME-16');

	await setCurrentTime(page, 10);
	await copy.click();
	await expect.poll(() => clipboardCalls(page)).toHaveLength(1);

	// Change only the media time: the next copy must read it fresh.
	await setCurrentTime(page, 50.2);
	await copy.click();
	await expect.poll(() => clipboardCalls(page)).toHaveLength(2);

	await page.getByTestId('queue-select-third-light').click();
	await setCurrentTime(page, 5.9);
	await copy.click();
	await expect.poll(() => clipboardCalls(page)).toHaveLength(3);

	const calls = await clipboardCalls(page);
	expectShareUrl(calls[0] ?? '', { track: 'demo-track', t: 10 }, page.url());
	expectShareUrl(calls[1] ?? '', { track: 'demo-track', t: 50 }, page.url());
	expectShareUrl(calls[2] ?? '', { track: 'third-light', t: 5 }, page.url());
});

test('[COPY-TIME-17] copy scope stays within the synthetic demo ids on /demo/player', async ({
	page
}) => {
	await openPlayer(page);
	const copy = await expectCopyButton(page, 'COPY-TIME-17');

	for (const id of DEMO_IDS) {
		await page.getByTestId(`queue-select-${id}`).click();
		await setCurrentTime(page, 12);
		await copy.click();
	}
	await expect.poll(() => clipboardCalls(page)).toHaveLength(DEMO_IDS.length);
	const calls = await clipboardCalls(page);
	for (let i = 0; i < DEMO_IDS.length; i += 1) {
		const id = DEMO_IDS[i] ?? '';
		expectShareUrl(calls[i] ?? '', { track: id, t: 12 }, page.url());
		expect(calls[i] ?? '', 'result must stay on the demo player').toContain('/demo/player');
		expect(calls[i] ?? '', 'result must not point at the library').not.toContain('/library');
	}
});

test('[COPY-TIME-18] invalid activation clears a prior success instead of leaving it stale', async ({
	page
}) => {
	await openPlayer(page);
	const copy = await expectCopyButton(page, 'COPY-TIME-18');

	// A valid activation first: success is visible.
	await setCurrentTime(page, 20.4);
	await copy.click();
	await expect(page.getByTestId('share-copy-status')).toContainText(/copied/i);

	// The track stays recognized so the action stays enabled; only the media
	// time turns invalid, so activation must refuse AND clear the prior result.
	await mockCurrentTime(page, Number.NaN);
	await expect(copy, 'COPY-TIME-18: the action stays enabled with a recognized track').toBeEnabled();
	await copy.click();
	await page.waitForTimeout(200);
	expect(
		await clipboardCalls(page),
		'COPY-TIME-18: the refusing activation must not write the clipboard'
	).toHaveLength(1);
	await expect(page.getByTestId('share-copy-status')).not.toContainText(/copied|success/i);
	expect(
		await page.getByTestId('share-copy-fallback').count(),
		'COPY-TIME-18: refusal must not leave a fallback behind'
	).toBe(0);
});

test('[COPY-TIME-19] a refusing activation supersedes a pending write: late completion stays ignored', async ({
	page
}) => {
	await openPlayer(page, { clipboard: 'delayed' });
	const copy = await expectCopyButton(page, 'COPY-TIME-19');

	// A valid activation with a pending clipboard write: no success yet.
	await setCurrentTime(page, 10);
	await copy.click();
	await expect(page.getByTestId('share-copy-status')).not.toContainText(/copied/i);

	// A refusing activation (invalid time, recognized track) must supersede
	// the pending write, so its late completion is ignored.
	await mockCurrentTime(page, Number.NaN);
	await expect(copy, 'COPY-TIME-19: the action stays enabled with a recognized track').toBeEnabled();
	await copy.click();
	await resolveDelayedWrite(page);
	await page.waitForTimeout(200);
	await expect(
		page.getByTestId('share-copy-status'),
		'late completion from a write superseded by a refusal must be ignored'
	).not.toContainText(/copied|success/i);
	expect(
		await page.getByTestId('share-copy-fallback').count(),
		'COPY-TIME-19: no stale fallback from the superseded write'
	).toBe(0);
	expect(
		await clipboardCalls(page),
		'COPY-TIME-19: the refusing activation must not write the clipboard'
	).toHaveLength(1);
});
