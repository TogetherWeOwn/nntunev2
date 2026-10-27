import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Acceptance: confirm route exits that would stop active listening.
 *
 * OWNERSHIP: this file is the single protected Test Author bank for this
 * feature. The Builder continues on this exact branch and must make these
 * tests pass WITHOUT editing this file; a wrong test comes back to the
 * acceptance card instead. No test imports project code, so a failure is
 * never an import/compile error: every failing `expect` names the expected
 * behavior, the actual state and the Builder's next action.
 *
 * FEATURE (build contract, self-contained):
 * Leaving a playback route tears the player down and silently ends the
 * listening session. On the two routes that own actual audio today —
 * `/demo/player` (synthetic demo player) and `/library` (native audio for
 * a queued track that carries a stream URL) — an in-app navigation that
 * would leave the route must first offer one clear choice, but ONLY while
 * the current actual audio has entered `playing` and still retains
 * playback intent. This is a route-local warning using the SvelteKit
 * navigation lifecycle, not a new player shell and not persistent
 * playback across routes.
 *
 * LOCKED SURFACE (so the Builder never guesses):
 * - The confirmation is a modal dialog element (`<dialog>` or an element
 *   with role `dialog`/`alertdialog`) containing EXACTLY two buttons,
 *   with exact accessible names `Leave and stop listening` and `Stay`,
 *   plus explanatory text (outside the buttons) saying that leaving
 *   stops listening/playback. It never claims to protect a tab, window
 *   or browser close, and contains no assertive live region / alert.
 * - Focus starts on `Stay` (the safe choice). Tab/Shift+Tab stay inside
 *   the dialog. `Escape` means Stay. Stay returns focus to the element
 *   that was focused when the exit was attempted (the shell link for a
 *   link exit; the previously focused control for a Back exit).
 * - Only an entered-`playing`, intent-retaining CURRENT audio arms the
 *   confirmation. Selected/metadata-only, loading (play not yet
 *   entered), paused (transport key or native pause), ended, failed and
 *   empty states navigate immediately with no dialog. Events from a
 *   detached previous source never arm it.
 * - Stay changes nothing: same audio element, same source, same time,
 *   same queue/current track, no extra play()/pause() calls, same URL.
 *   Leave performs the attempted navigation exactly once through normal
 *   route teardown; a later Back/Forward lands on a fresh, non-playing
 *   mount and never re-prompts. Leaving after a Back attempt resumes
 *   that history move (Forward still returns to the player route), it
 *   does not push a duplicate destination entry.
 * - The pending destination is bound to the navigation that opened the
 *   dialog: Stay-then-Leave toward a second destination lands on the
 *   second, never the first. Rapid/duplicate attempts open one dialog.
 * - Same-route hash changes, clicks that open a new tab, and exits from
 *   a route whose audio has already been torn down by an auth
 *   transition keep native/immediate behavior. Sign-out, expired-session
 *   (401) recovery and cross-tab auth-transition teardown are never
 *   delayed by this confirmation. No `beforeunload` trap is added to a
 *   paused page. No new storage keys, no mutating/off-origin requests,
 *   and transport shortcuts do not leak through the open dialog.
 *
 * HARNESS (established patterns, synthetic media only):
 * - Demo: media route held pending, `play()` stubbed, so playback is
 *   driven by the app's own status (`Status: playing` etc.) plus real
 *   `play`/`playing`/`pause` events on the current element (the
 *   demo-tab-title / player-lifecycle pattern).
 * - Library: `/api/library` is route-mocked; a track with a `streamUrl`
 *   reached through the existing `/library?track=<id>&t=<s>` share link
 *   renders the existing native `audio-player` paused (the
 *   library-share-link-lifecycle pattern). "Entering playing" is the
 *   native-control signal: `play()` plus `play`/`playing` events.
 * - Client-side entry for Back cases uses a same-origin anchor click so
 *   the real router owns both history entries (the tab-title-lifecycle
 *   pattern); `page.goto` document loads are not router Back exits.
 *
 * RED vs GREEN on the shipped base (no confirmation exists yet):
 * - RED (feature missing): NS-D01, NS-D09..NS-D15, NS-D18, NS-L03,
 *   NS-L04, NS-L06, NS-L09, NS-P01, NS-P02. Each fails first on the
 *   missing one-dialog expectation in `requireConfirmation`.
 * - GREEN controls (must pass before and after): NS-00, NS-D02..D08,
 *   NS-D16, NS-D17, NS-D19, NS-L01, NS-L02, NS-L05, NS-L07, NS-L08,
 *   NS-A01. These separate harness/baseline failure from feature RED
 *   and fence the Builder against over-prompting.
 *
 * Deliberately NOT pinned (documented so nobody invents a requirement):
 * buffering/`waiting` after entered playing; document unload, tab close
 * or reload while playing (browser-close guarantees are out of scope);
 * backdrop-click dismissal; exact dialog wording beyond the locked
 * button names and the stops-listening explanation; pause of the audio
 * while the dialog is merely open (playback may continue behind it).
 *
 * CASE INDEX:
 *   NS-00   harness control: demo + library audio fixtures settle
 *   NS-D01  demo playing + shell exit prompts once, URL unchanged
 *   NS-D02  demo selected-only exits without a prompt
 *   NS-D03  demo paused via transport key exits without a prompt
 *   NS-D04  demo loading (play never entered) exits without a prompt
 *   NS-D05  demo ended exits without a prompt
 *   NS-D06  demo playback error exits without a prompt
 *   NS-D07  demo empty queue exits without a prompt
 *   NS-D08  paused current + stale detached-source events: no prompt
 *   NS-D09  Stay preserves audio, queue, URL and focus (shell exit)
 *   NS-D10  Leave navigates exactly once; Back/Forward remount fresh
 *   NS-D11  Back: Stay preserves, second Back + Leave, Forward fresh
 *   NS-D12  keyboard open, focus trap, Escape=Stay, keyboard Leave
 *   NS-D13  transport keys do not leak through the dialog; no assertive
 *   NS-D14  second destination wins after a Stay (no stale resume)
 *   NS-D15  rapid double attempt opens one dialog, Leave lands once
 *   NS-D16  same-route hash / same-URL navigation never prompts
 *   NS-D17  modifier-click new tab keeps the original playing, no prompt
 *   NS-D18  source change while playing: prompt protects the new track
 *   NS-D19  paused page: document navigation has no beforeunload trap
 *   NS-L01  library metadata-only queue exits without a prompt
 *   NS-L02  library stream audio never played exits without a prompt
 *   NS-L03  library playing: prompt + Stay preserves everything
 *   NS-L04  library playing: Leave exits once; Back remounts fresh/silent
 *   NS-L05  library native pause exits without a prompt
 *   NS-L06  library source change + playing: Stay protects new source
 *   NS-L07  auth transition tears down at once, never prompts
 *   NS-L08  expired session (401): sign-in recovery is never prompted
 *   NS-L09  library Back: Stay preserves; paused Back then just leaves
 *   NS-A01  account sign-out is never delayed by a leave prompt
 *   NS-P01  narrow light/dark dialog presentation + screenshots
 *   NS-P02  Stay adds no storage keys and no mutating/off-origin calls
 *
 * Run focused (from the repo root):
 *   pnpm --dir apps/web exec playwright test \
 *     tests/player-route-exit-confirm.spec.ts --workers=1 --retries=0
 */

const PRESENCE_TIMEOUT = 3_000;
const BUILDER =
	'Builder: add the route-local SvelteKit-navigation confirmation described in this file header; do not edit this protected bank.';

const LIBRARY_STREAM_ITEMS = [
	{ id: 'coral', title: 'Coral Night', artist: 'Exit Band', streamUrl: '/test-audio/coral' },
	{ id: 'pine', title: 'Pine Morning', artist: 'Exit Band', streamUrl: '/test-audio/pine' }
];

const LIBRARY_META_ITEMS = [
	{ id: 'meta-one', title: 'Metadata One', artist: 'Exit Band' },
	{ id: 'meta-two', title: 'Metadata Two', artist: 'Exit Band' }
];

type ProbeWindow = Window & { __routeExit: { playCalls: number; pauseCalls: number } };

// ---------------------------------------------------------------------------
// Evidence receipt: one compact JSON line per case in the run log/report.
// ---------------------------------------------------------------------------

test.afterEach(async ({}, testInfo) => {
	const caseId = /\[NS-[A-Z0-9-]+\]/.exec(testInfo.title)?.[0]?.slice(1, -1) ?? 'NS-UNKNOWN';
	const failed = testInfo.errors.length > 0 || testInfo.status === 'failed';
	const receipt = {
		case_id: caseId,
		expected: testInfo.title.replace(/\[NS-[A-Z0-9-]+\]\s*/, ''),
		actual: failed
			? testInfo.errors.map((error) => String(error.message).split('\n')[0]).join(' | ')
			: 'expectations met',
		status: failed ? 'RED' : 'GREEN',
		error_code: failed ? 'ROUTE_EXIT_CONFIRM_CONTRACT' : null,
		next_action: failed
			? 'Builder implements the header contract; acceptance bank unchanged'
			: 'preserve acceptance'
	};
	const body = JSON.stringify(receipt);
	console.log(body);
	await testInfo.attach('acceptance-case.json', { body, contentType: 'application/json' });
});

// ---------------------------------------------------------------------------
// Shared locators
// ---------------------------------------------------------------------------

/** The app shell nav, located by the link it owns (app-shell pattern). */
function shellNav(page: Page): Locator {
	return page.locator('nav:has(a[href="/library/import"])');
}

function libraryLink(page: Page): Locator {
	return shellNav(page).getByRole('link', { name: 'Library', exact: true });
}

function importLink(page: Page): Locator {
	return shellNav(page).getByRole('link', { name: 'Select NZB locally', exact: true });
}

function leaveButton(page: Page): Locator {
	return page.getByRole('button', { name: 'Leave and stop listening', exact: true });
}

function stayButton(page: Page): Locator {
	return page.getByRole('button', { name: 'Stay', exact: true });
}

/** The one modal confirmation: dialog semantics owning the Leave action. */
function confirmation(page: Page): Locator {
	return page
		.locator('dialog[open], [role="alertdialog"], [role="dialog"]')
		.filter({ has: leaveButton(page) });
}

/** The fast, named RED gate every prompt case reaches first. */
async function requireConfirmation(page: Page, where: string): Promise<void> {
	await expect(
		confirmation(page),
		`expected exactly one modal confirmation (a <dialog> or role dialog/alertdialog ` +
			`owning a "Leave and stop listening" button) when exiting ${where} during active ` +
			`playing; got a silent navigation or no dialog. ${BUILDER}`
	).toHaveCount(1, { timeout: PRESENCE_TIMEOUT });
	await expect(
		leaveButton(page),
		`expected exactly one "Leave and stop listening" button at ${where}. ${BUILDER}`
	).toHaveCount(1, { timeout: PRESENCE_TIMEOUT });
	await expect(
		stayButton(page),
		`expected exactly one "Stay" button at ${where}. ${BUILDER}`
	).toHaveCount(1, { timeout: PRESENCE_TIMEOUT });
}

/** A no-prompt exit: lands on the destination and never showed the choice. */
async function expectUnpromptedExit(page: Page, url: RegExp, where: string): Promise<void> {
	await expect(
		page,
		`expected exiting ${where} to navigate immediately to ${url} with no confirmation; ` +
			`actual URL is ${page.url()}. Builder: only an entered-playing, intent-retaining ` +
			'current audio may prompt.'
	).toHaveURL(url);
	await expect(
		leaveButton(page),
		`expected no "Leave and stop listening" prompt for ${where}. ` +
			'Builder: do not arm the confirmation outside active playing.'
	).toHaveCount(0);
	await expect(
		stayButton(page),
		`expected no "Stay" prompt for ${where}. Builder: keep the choice out of this state.`
	).toHaveCount(0);
}

async function mockLibrary(page: Page, body: unknown): Promise<void> {
	await page.route('**/api/library', (route) =>
		route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(body) })
	);
}

/** Same-document client navigation through a real anchor (router-owned). */
async function anchorNavigate(page: Page, path: string): Promise<void> {
	await page.evaluate((href) => {
		const link = document.createElement('a');
		link.id = 'route-exit-navigation';
		link.href = href;
		link.textContent = 'Navigate';
		document.body.append(link);
	}, path);
	await page.locator('#route-exit-navigation').click();
	await expect(page).toHaveURL(new RegExp(`${path.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}$`));
	await page.locator('#route-exit-navigation').evaluate((link) => link.remove());
}

// ---------------------------------------------------------------------------
// Demo player harness
// ---------------------------------------------------------------------------

async function openDemo(
	page: Page,
	mode: 'resolve' | 'reject' | 'pending' = 'resolve',
	destinationLibrary: unknown = []
): Promise<void> {
	await page.route('**/api/demo/audio**', () => {});
	await mockLibrary(page, destinationLibrary);
	await page.addInitScript((playMode) => {
		const probe = { playCalls: 0, pauseCalls: 0 };
		(window as unknown as ProbeWindow).__routeExit = probe;
		HTMLMediaElement.prototype.play = function () {
			probe.playCalls += 1;
			if (playMode === 'reject') return Promise.reject(new Error('fixture boom'));
			if (playMode === 'pending') return new Promise<void>(() => {});
			Object.defineProperty(this, 'paused', { value: false, configurable: true });
			return Promise.resolve();
		};
		HTMLMediaElement.prototype.pause = function () {
			probe.pauseCalls += 1;
			Object.defineProperty(this, 'paused', { value: true, configurable: true });
		};
	}, mode);
	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(page.getByTestId('player-status')).toHaveText('Status: selected');
}

/** Enter playing the way both state-machine and media-event gates observe. */
async function startDemoPlaying(page: Page, selectTestId = 'queue-select-second-signal'): Promise<void> {
	await page.getByTestId(selectTestId).click();
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await page.getByTestId('demo-audio').evaluate((el) => {
		Object.defineProperty(el, 'paused', { value: false, configurable: true });
		Object.defineProperty(el, 'ended', { value: false, configurable: true });
		el.dispatchEvent(new Event('play'));
		el.dispatchEvent(new Event('playing'));
	});
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
}

async function demoSnapshot(page: Page): Promise<{
	src: string | null;
	time: number;
	playCalls: number;
	pauseCalls: number;
	nowPlaying: string;
	queue: (string | null)[];
}> {
	const audio = page.getByTestId('demo-audio');
	return {
		src: await audio.getAttribute('src'),
		time: await audio.evaluate((el: HTMLAudioElement) => el.currentTime),
		...(await page.evaluate(() => {
			const probe = (window as unknown as ProbeWindow).__routeExit;
			return { playCalls: probe.playCalls, pauseCalls: probe.pauseCalls };
		})),
		nowPlaying: (await page.getByTestId('now-playing').textContent())?.trim() ?? '',
		queue: await page.getByTestId('queue-item').evaluateAll((items) =>
			items.map((item) => item.getAttribute('data-track-id') ?? item.getAttribute('data-id'))
		)
	};
}

/** Pin a synthetic playback position and identity marker on the live element. */
async function tagAudioTime(locator: Locator, seconds: number): Promise<void> {
	await locator.evaluate((el: HTMLAudioElement, value) => {
		let time = value;
		Object.defineProperty(el, 'currentTime', {
			configurable: true,
			get: () => time,
			set: (next: number) => {
				time = next;
			}
		});
		el.dataset.routeExitMarker = 'live-element';
	}, seconds);
}

// ---------------------------------------------------------------------------
// Library harness
// ---------------------------------------------------------------------------

async function openLibraryStream(page: Page, path = '/library?track=coral&t=90'): Promise<void> {
	await page.route('**/api/library', (route) => route.fulfill({ json: LIBRARY_STREAM_ITEMS }));
	await page.route('**/test-audio/**', () => {});
	await page.addInitScript(() => {
		const probe = { playCalls: 0, pauseCalls: 0 };
		(window as unknown as ProbeWindow).__routeExit = probe;
		HTMLMediaElement.prototype.play = function () {
			probe.playCalls += 1;
			Object.defineProperty(this, 'paused', { value: false, configurable: true });
			return Promise.resolve();
		};
		HTMLMediaElement.prototype.pause = function () {
			probe.pauseCalls += 1;
			Object.defineProperty(this, 'paused', { value: true, configurable: true });
		};
	});
	await page.goto(path);
	await expect(page.getByTestId('library-item')).toHaveCount(LIBRARY_STREAM_ITEMS.length);
	await expect(page.getByTestId('audio-player')).toHaveCount(1);
	await expect(page.locator('[data-testid="queue-item"][aria-current="true"]')).toHaveAttribute(
		'data-track-id',
		'coral'
	);
	await expect(page.getByTestId('audio-player')).toHaveJSProperty('paused', true);
}

/** Native-control resume: play() plus the events a real element fires. */
async function startLibraryPlaying(page: Page): Promise<void> {
	await page.getByTestId('audio-player').evaluate((el: HTMLAudioElement) => {
		void el.play();
		Object.defineProperty(el, 'paused', { value: false, configurable: true });
		Object.defineProperty(el, 'ended', { value: false, configurable: true });
		el.dispatchEvent(new Event('play'));
		el.dispatchEvent(new Event('playing'));
	});
	await expect(page.getByTestId('audio-player')).toHaveJSProperty('paused', false);
}

async function pauseLibraryNative(page: Page): Promise<void> {
	await page.getByTestId('audio-player').evaluate((el: HTMLAudioElement) => {
		el.pause();
		Object.defineProperty(el, 'paused', { value: true, configurable: true });
		el.dispatchEvent(new Event('pause'));
	});
	await expect(page.getByTestId('audio-player')).toHaveJSProperty('paused', true);
}

function libraryRow(page: Page, id: string): Locator {
	return page.locator(`[data-testid="library-item"][data-id="${id}"] button.nt-library-row`);
}

// ---------------------------------------------------------------------------
// Harness control
// ---------------------------------------------------------------------------

test('[NS-00] fixtures settle: demo reaches playing and library audio starts paused (GREEN control)', async ({
	page
}) => {
	await openDemo(page);
	await startDemoPlaying(page);
	await expect(page.getByTestId('demo-audio')).toHaveJSProperty('paused', false);

	await openLibraryStream(page);
	await expect(page.getByTestId('now-playing')).toContainText('Coral Night');
	await startLibraryPlaying(page);
	await pauseLibraryNative(page);
});

// ---------------------------------------------------------------------------
// Demo gating: who prompts and who never does
// ---------------------------------------------------------------------------

test('[NS-D01] demo playing + shell Library exit offers the one clear choice and stays put', async ({
	page
}) => {
	await openDemo(page);
	await startDemoPlaying(page);

	await libraryLink(page).click();
	await requireConfirmation(page, 'the demo player');

	expect(
		page.url(),
		`expected the URL to still be the demo player while the choice is open; got ${page.url()}. ` +
			'Builder: cancel the navigation before asking, resume it only on Leave.'
	).toContain('/demo/player');
	await expect(
		page.getByTestId('player-status'),
		'expected playback state to survive opening the confirmation. Builder: asking must not pause or tear down.'
	).toHaveText('Status: playing');

	const dialog = confirmation(page);
	await expect(
		dialog,
		`expected the confirmation to carry an accessible name. ${BUILDER}`
	).toHaveAccessibleName(/.+/);
	await expect(
		dialog.getByRole('button'),
		'expected exactly the two locked choices inside the confirmation (Leave and stop listening / Stay). ' +
			'Builder: one clear two-way choice, no extra actions.'
	).toHaveCount(2);
	const explanation = await dialog.evaluate((element) => {
		const clone = element.cloneNode(true) as HTMLElement;
		clone.querySelectorAll('button').forEach((button) => button.remove());
		return (clone.textContent ?? '').replace(/\s+/g, ' ').trim();
	});
	expect(
		explanation,
		`expected explanatory text (outside the buttons) saying leaving stops listening/playback; got "${explanation}". ${BUILDER}`
	).toMatch(/stop|end/i);
	expect(explanation).toMatch(/listen|playback|audio|track/i);
	const dialogText = (await dialog.innerText()).replace(/\s+/g, ' ');
	expect(
		dialogText,
		'expected no claim that this protects a tab/window/browser close (out of scope, and a native unload prompt cannot show custom text). ' +
			'Builder: describe only the in-app route exit.'
	).not.toMatch(/clos(e|ing) (this |the )?(tab|browser|window)/i);

	// Clean exit from the case without leaving a dialog behind.
	await stayButton(page).click();
	await expect(confirmation(page)).toHaveCount(0);
});

test('[NS-D02] demo selected-only (never played) exits without a prompt (GREEN control)', async ({
	page
}) => {
	await openDemo(page);
	await expect(page.getByTestId('player-status')).toHaveText('Status: selected');

	await libraryLink(page).click();
	await expectUnpromptedExit(page, /\/library$/, 'the demo player while merely selected');
});

test('[NS-D03] demo paused via transport key exits without a prompt (GREEN control)', async ({
	page
}) => {
	await openDemo(page);
	await startDemoPlaying(page);
	await page.keyboard.press('k');
	await expect(page.getByTestId('player-status')).toHaveText('Status: paused');

	await libraryLink(page).click();
	await expectUnpromptedExit(page, /\/library$/, 'the demo player while paused');
});

test('[NS-D04] demo loading (play requested, playing never entered) exits without a prompt (GREEN control)', async ({
	page
}) => {
	await openDemo(page, 'pending');
	await page.getByTestId('queue-select-second-signal').click();
	await expect(page.getByTestId('player-status')).toHaveText('Status: loading');

	await libraryLink(page).click();
	await expectUnpromptedExit(page, /\/library$/, 'the demo player while still loading');
});

test('[NS-D05] demo ended exits without a prompt (GREEN control)', async ({ page }) => {
	await openDemo(page);
	await startDemoPlaying(page, 'queue-select-third-light');
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('player-status')).toHaveText('Status: ended');

	await libraryLink(page).click();
	await expectUnpromptedExit(page, /\/library$/, 'the demo player after the queue ended');
});

test('[NS-D06] demo playback error exits without a prompt (GREEN control)', async ({ page }) => {
	await openDemo(page, 'reject');
	await page.getByTestId('queue-select-second-signal').click();
	await expect(page.getByTestId('player-error')).toBeVisible();

	await libraryLink(page).click();
	await expectUnpromptedExit(page, /\/library$/, 'the demo player in the error state');
});

test('[NS-D07] demo empty queue exits without a prompt (GREEN control)', async ({ page }) => {
	await openDemo(page);
	await page.getByTestId('queue-clear').click();
	await expect(page.getByTestId('player-status')).toHaveText('Status: empty');

	await libraryLink(page).click();
	await expectUnpromptedExit(page, /\/library$/, 'the demo player with an empty queue');
});

test('[NS-D08] paused current track plus stale detached-source events never prompt (GREEN control)', async ({
	page
}) => {
	await openDemo(page);
	await startDemoPlaying(page, 'queue-select-demo-track');
	const oldSource = await page.getByTestId('demo-audio').elementHandle();
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await page.keyboard.press('k');
	await expect(page.getByTestId('player-status')).toHaveText('Status: paused');

	// Events on the detached previous element are not the current audio.
	for (const type of ['play', 'playing']) {
		await oldSource!.evaluate((el, name) => {
			Object.defineProperty(el, 'paused', { value: false, configurable: true });
			el.dispatchEvent(new Event(name));
		}, type);
	}
	await expect(page.getByTestId('player-status')).toHaveText('Status: paused');

	await libraryLink(page).click();
	await expectUnpromptedExit(
		page,
		/\/library$/,
		'the demo player when only a detached old source claims playing'
	);
});

// ---------------------------------------------------------------------------
// Demo Stay / Leave outcomes
// ---------------------------------------------------------------------------

test('[NS-D09] Stay preserves the live audio, time, queue, URL and trigger focus', async ({
	page
}) => {
	await openDemo(page);
	await startDemoPlaying(page);
	await tagAudioTime(page.getByTestId('demo-audio'), 12.5);
	const liveElement = await page.getByTestId('demo-audio').elementHandle();
	const before = await demoSnapshot(page);
	expect(before.time).toBe(12.5);

	await libraryLink(page).focus();
	await expect(libraryLink(page)).toBeFocused();
	await libraryLink(page).click();
	await requireConfirmation(page, 'the demo player');

	await stayButton(page).click();
	await expect(
		confirmation(page),
		'expected Stay to close the confirmation. Builder: Stay is local dismissal only.'
	).toHaveCount(0);
	expect(
		page.url(),
		`expected Stay to keep the demo player URL; got ${page.url()}. Builder: the cancelled navigation must not leak through later.`
	).toContain('/demo/player');
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');

	const after = await demoSnapshot(page);
	expect(
		after,
		'expected Stay to preserve source, time, play/pause call counts, now-playing and queue exactly. ' +
			'Builder: Stay must not remount, reseek, pause or restart the audio.'
	).toEqual(before);
	expect(
		await liveElement!.evaluate(
			(el) => el.isConnected && el === document.querySelector('[data-testid="demo-audio"]')
		),
		'expected the same audio element instance after Stay. Builder: keep element identity.'
	).toBe(true);
	await expect(page.getByTestId('demo-audio')).toHaveAttribute(
		'data-route-exit-marker',
		'live-element'
	);
	await expect(
		libraryLink(page),
		'expected focus to return to the shell link that attempted the exit after Stay. ' +
			'Builder: restore focus to the pre-dialog element.'
	).toBeFocused();
});

test('[NS-D10] Leave navigates exactly once with normal teardown; Back and Forward remount fresh', async ({
	page
}) => {
	await openDemo(page);
	await startDemoPlaying(page);
	const liveElement = await page.getByTestId('demo-audio').elementHandle();

	await libraryLink(page).click();
	await requireConfirmation(page, 'the demo player');
	await leaveButton(page).click();

	await expect(page, 'expected Leave to land on the attempted shell destination once.').toHaveURL(
		/\/library$/
	);
	await expect(confirmation(page)).toHaveCount(0);
	await expect(
		page.getByTestId('demo-audio'),
		'expected normal demo teardown after Leave (no lingering live player on the destination). ' +
			'Builder: Leave is an ordinary route exit, not a hidden persistent player.'
	).toHaveCount(0);
	expect(
		await liveElement!.evaluate((el) => el.isConnected),
		'expected the old audio element detached after Leave. Builder: run the existing teardown.'
	).toBe(false);

	// Exactly one destination entry: Back returns to a fresh player mount.
	await page.goBack();
	await expect(page).toHaveURL(/\/demo\/player$/);
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(
		page.getByTestId('player-status'),
		'expected Back after Leave to remount non-playing (no resumed session, no prompt). ' +
			'Builder: playback does not survive a Leave.'
	).toHaveText('Status: selected');
	await expect(leaveButton(page)).toHaveCount(0);

	// Forward exits the fresh (non-playing) mount without prompting.
	await page.goForward();
	await expect(page).toHaveURL(/\/library$/);
	await expect(leaveButton(page)).toHaveCount(0);
});

test('[NS-D11] Back offers Stay that preserves playback, a second Back + Leave resumes history, Forward is fresh', async ({
	page
}) => {
	await mockLibrary(page, []);
	await page.route('**/api/demo/audio**', () => {});
	await page.addInitScript(() => {
		const probe = { playCalls: 0, pauseCalls: 0 };
		(window as unknown as ProbeWindow).__routeExit = probe;
		HTMLMediaElement.prototype.play = function () {
			probe.playCalls += 1;
			Object.defineProperty(this, 'paused', { value: false, configurable: true });
			return Promise.resolve();
		};
		HTMLMediaElement.prototype.pause = function () {
			probe.pauseCalls += 1;
			Object.defineProperty(this, 'paused', { value: true, configurable: true });
		};
	});
	await page.goto('/');
	await anchorNavigate(page, '/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await startDemoPlaying(page);
	await tagAudioTime(page.getByTestId('demo-audio'), 7.25);
	const before = await demoSnapshot(page);

	await page.getByTestId('player-next').focus();
	await expect(page.getByTestId('player-next')).toBeFocused();
	await page.goBack();
	await requireConfirmation(page, 'the demo player via Back');

	await stayButton(page).click();
	await expect(
		page,
		'expected Back + Stay to restore the demo player URL. Builder: a cancelled popstate must leave URL and history usable.'
	).toHaveURL(/\/demo\/player$/);
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	expect(
		await demoSnapshot(page),
		'expected Back + Stay to preserve source, time, queue and transport counts. Builder: Stay after Back is a no-op for playback.'
	).toEqual(before);
	await expect(
		page.getByTestId('player-next'),
		'expected focus back on the control focused before Back. Builder: restore pre-dialog focus.'
	).toBeFocused();

	// Stay is per-attempt: the next Back asks again, never silently resumes.
	await page.goBack();
	await requireConfirmation(page, 'the demo player via a second Back');
	await leaveButton(page).click();
	await expect(
		page,
		'expected Back + Leave to land on the previous history entry (the landing route). ' +
			'Builder: resume the attempted history move, not a fresh duplicate navigation.'
	).toHaveURL(new RegExp(`^${new URL(page.url()).origin}/$`));
	await expect(page.getByTestId('demo-audio')).toHaveCount(0);
	await expect(leaveButton(page)).toHaveCount(0);

	// The history move was real: Forward returns to a fresh, silent player.
	await page.goForward();
	await expect(page).toHaveURL(/\/demo\/player$/);
	await expect(page.getByTestId('player-status')).toHaveText('Status: selected');
	await expect(leaveButton(page)).toHaveCount(0);
});

test('[NS-D12] keyboard: Enter opens, focus starts on Stay and traps, Escape stays, keyboard Leave works', async ({
	page
}) => {
	await openDemo(page);
	await startDemoPlaying(page);

	await libraryLink(page).focus();
	await page.keyboard.press('Enter');
	await requireConfirmation(page, 'the demo player via keyboard');

	await expect(
		stayButton(page),
		'expected initial dialog focus on Stay (the safe choice). Builder: never focus the destructive Leave by default.'
	).toBeFocused();
	await page.keyboard.press('Tab');
	await expect(
		leaveButton(page),
		'expected Tab to move Stay -> Leave inside the dialog. Builder: keep a modal focus trap.'
	).toBeFocused();
	await page.keyboard.press('Tab');
	await expect(
		stayButton(page),
		'expected Tab on the last control to wrap to Stay (focus never escapes to the page behind). Builder: modal trap.'
	).toBeFocused();
	await page.keyboard.press('Shift+Tab');
	await expect(
		leaveButton(page),
		'expected Shift+Tab on the first control to wrap to Leave. Builder: keep the reverse trap.'
	).toBeFocused();

	await page.keyboard.press('Escape');
	await expect(
		confirmation(page),
		'expected Escape to close the confirmation as Stay. Builder: Escape never means Leave.'
	).toHaveCount(0);
	expect(page.url()).toContain('/demo/player');
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await expect(
		libraryLink(page),
		'expected focus restored to the triggering shell link after Escape. Builder: restore pre-dialog focus.'
	).toBeFocused();

	// Reopen and leave entirely by keyboard.
	await page.keyboard.press('Enter');
	await requireConfirmation(page, 'the demo player via keyboard, reopened');
	await expect(stayButton(page)).toBeFocused();
	await page.keyboard.press('Tab');
	await expect(leaveButton(page)).toBeFocused();
	await page.keyboard.press('Enter');
	await expect(page).toHaveURL(/\/library$/);
	await expect(confirmation(page)).toHaveCount(0);

	// Focus must not be stranded on the removed dialog, and sequential
	// keyboard navigation must work on the destination (whether the
	// router lands focus on the main landmark or at the document top).
	const landedConnected = await page.evaluate(
		() => document.activeElement === document.body || (document.activeElement?.isConnected ?? false)
	);
	expect(
		landedConnected,
		'expected focus on a live destination element after a keyboard Leave. Builder: never leave focus on the removed dialog.'
	).toBe(true);
	await page.keyboard.press('Tab');
	const tabbedTag = await page.evaluate(() => document.activeElement?.tagName ?? null);
	expect(
		tabbedTag !== null && tabbedTag !== 'BODY',
		`expected Tab on the destination to move focus to a real control; active element is <${tabbedTag}>. ` +
			'Builder: keyboard navigation must be usable immediately after Leave.'
	).toBe(true);
});

test('[NS-D13] transport shortcuts do not leak through the open confirmation and it is not assertive', async ({
	page
}) => {
	await openDemo(page);
	await startDemoPlaying(page);
	const before = await demoSnapshot(page);

	await libraryLink(page).click();
	await requireConfirmation(page, 'the demo player');

	await page.keyboard.press('k');
	await page.keyboard.press('n');
	const assertiveInside = await confirmation(page).evaluate(
		(element) => element.querySelectorAll('[aria-live="assertive"], [role="alert"]').length
	);
	expect(
		assertiveInside,
		'expected no assertive live region or alert inside the confirmation. Builder: announce once through the dialog itself, never assertive spam.'
	).toBe(0);

	await stayButton(page).click();
	const after = await demoSnapshot(page);
	expect(
		after,
		'expected "k"/"n" pressed while the dialog was open to change nothing (no pause, no track advance, no new transport calls). ' +
			'Builder: the open confirmation is modal; global player keys must not leak through.'
	).toEqual(before);
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
});

test('[NS-D14] after a Stay, Leave toward a second destination lands there, never on the stale first one', async ({
	page
}) => {
	await openDemo(page);
	await startDemoPlaying(page);

	await libraryLink(page).click();
	await requireConfirmation(page, 'the demo player toward Library');
	await stayButton(page).click();
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');

	await importLink(page).click();
	await requireConfirmation(page, 'the demo player toward Select NZB locally');
	await leaveButton(page).click();

	await expect(
		page,
		'expected Leave to resume the SECOND attempted destination (/library/import), not the stale first one (/library). ' +
			'Builder: bind the continuation to the navigation that opened this dialog (current generation only).'
	).toHaveURL(/\/library\/import$/);
	await expect(page.getByTestId('import-file')).toBeVisible();
	await expect(leaveButton(page)).toHaveCount(0);
});

test('[NS-D15] a rapid double attempt opens exactly one dialog and Leave lands exactly once', async ({
	page
}) => {
	await openDemo(page);
	await startDemoPlaying(page);

	// The second synthetic click may land on the modal top layer (or, on
	// the unbuilt base, after the first click already navigated away);
	// either way the one-dialog assertions below are the verdict.
	await libraryLink(page)
		.dblclick()
		.catch(() => {});
	await requireConfirmation(page, 'the demo player under a double click');
	await expect(
		confirmation(page),
		'expected a single confirmation under rapid duplicate attempts. Builder: one pending navigation, one dialog.'
	).toHaveCount(1);
	expect(page.url()).toContain('/demo/player');

	await leaveButton(page).click();
	await expect(page).toHaveURL(/\/library$/);
	await expect(leaveButton(page)).toHaveCount(0);

	// One navigation happened: a single Back returns to the player route.
	await page.goBack();
	await expect(
		page,
		'expected one Back after the rapid Leave to reach the demo player (no duplicate destination entries). ' +
			'Builder: Leave performs the attempted navigation exactly once.'
	).toHaveURL(/\/demo\/player$/);
	await expect(page.getByTestId('player-status')).toHaveText('Status: selected');
});

test('[NS-D16] same-route hash and same-URL navigation keep playing without a prompt (GREEN control)', async ({
	page
}) => {
	await openDemo(page);
	await startDemoPlaying(page);
	await tagAudioTime(page.getByTestId('demo-audio'), 3.5);
	const before = await demoSnapshot(page);

	await anchorNavigate(page, '/demo/player#route-exit-fixture');
	await expect(
		leaveButton(page),
		'expected a same-route hash change to keep native behavior with no prompt. Builder: hash-only moves are not route exits.'
	).toHaveCount(0);
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	expect(await demoSnapshot(page)).toEqual(before);

	await anchorNavigate(page, '/demo/player');
	await expect(leaveButton(page)).toHaveCount(0);
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	expect(await demoSnapshot(page)).toEqual(before);
});

test('[NS-D17] modifier-click opens the shell destination in a new tab; the original keeps playing (GREEN control)', async ({
	page,
	context
}) => {
	await openDemo(page);
	await startDemoPlaying(page);

	const [popup] = await Promise.all([
		context.waitForEvent('page'),
		libraryLink(page).click({ modifiers: ['ControlOrMeta'] })
	]);
	await popup.waitForLoadState();
	expect(
		popup.url(),
		`expected the new tab to open the Library destination natively; got ${popup.url()}. ` +
			'Builder: a new-tab open is not an exit of this tab and must not be intercepted.'
	).toContain('/library');
	await expect(leaveButton(popup)).toHaveCount(0);

	expect(
		page.url(),
		'expected the original tab to stay on the demo player after a modifier-click. Builder: do not cancel new-tab opens.'
	).toContain('/demo/player');
	await expect(
		leaveButton(page),
		'expected no confirmation in the original tab for a new-tab open. Builder: playback in this tab is not stopping.'
	).toHaveCount(0);
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await popup.close();
});

test('[NS-D18] changing track while playing re-arms on the new source; Stay preserves the new track', async ({
	page
}) => {
	await openDemo(page);
	await startDemoPlaying(page, 'queue-select-demo-track');
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await page.getByTestId('demo-audio').evaluate((el) => {
		Object.defineProperty(el, 'paused', { value: false, configurable: true });
		el.dispatchEvent(new Event('play'));
		el.dispatchEvent(new Event('playing'));
	});
	await tagAudioTime(page.getByTestId('demo-audio'), 5.75);
	const before = await demoSnapshot(page);

	await libraryLink(page).click();
	await requireConfirmation(page, 'the demo player after a track change');
	await stayButton(page).click();

	expect(
		await demoSnapshot(page),
		'expected Stay after a source change to preserve the NEW track/source/time, never the previous one. ' +
			'Builder: track the current actual audio, not the element that first played.'
	).toEqual(before);
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
});

test('[NS-D19] a paused demo page has no beforeunload trap on document navigation (GREEN control)', async ({
	page
}) => {
	const dialogs: string[] = [];
	page.on('dialog', (dialog) => {
		dialogs.push(dialog.type());
		void dialog.dismiss().catch(() => {});
	});
	await openDemo(page);
	await startDemoPlaying(page);
	await page.keyboard.press('k');
	await expect(page.getByTestId('player-status')).toHaveText('Status: paused');

	await page.goto('/library');
	await expect(page).toHaveURL(/\/library$/);
	expect(
		dialogs,
		`expected no native unload dialog when leaving a paused page; saw ${JSON.stringify(dialogs)}. ` +
			'Builder: never add a beforeunload trap to a paused page; the in-app choice is the whole feature.'
	).toEqual([]);
});

// ---------------------------------------------------------------------------
// Library route
// ---------------------------------------------------------------------------

test('[NS-L01] library metadata-only queue (no audio exists) exits without a prompt (GREEN control)', async ({
	page
}) => {
	await mockLibrary(page, LIBRARY_META_ITEMS);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(LIBRARY_META_ITEMS.length);
	await libraryRow(page, 'meta-one').click();
	await libraryRow(page, 'meta-two').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	await expect(page.getByTestId('now-playing')).toContainText('Metadata One');
	await expect(page.locator('audio')).toHaveCount(0);

	await importLink(page).click();
	await expectUnpromptedExit(
		page,
		/\/library\/import$/,
		'a library queue that is metadata only (selection is not listening)'
	);
});

test('[NS-L02] library stream audio that never entered playing exits without a prompt (GREEN control)', async ({
	page
}) => {
	await openLibraryStream(page);
	await expect(page.getByTestId('audio-player')).toHaveJSProperty('paused', true);

	await importLink(page).click();
	await expectUnpromptedExit(
		page,
		/\/library\/import$/,
		'the library while its native audio is present but paused'
	);
});

test('[NS-L03] library playing prompts once; Stay preserves the native audio, queue and focus', async ({
	page
}) => {
	await openLibraryStream(page);
	await startLibraryPlaying(page);
	await tagAudioTime(page.getByTestId('audio-player'), 42.5);
	const liveElement = await page.getByTestId('audio-player').elementHandle();
	const srcBefore = await page.getByTestId('audio-player').getAttribute('src');
	const callsBefore = await page.evaluate(() => {
		const probe = (window as unknown as ProbeWindow).__routeExit;
		return { playCalls: probe.playCalls, pauseCalls: probe.pauseCalls };
	});

	await importLink(page).focus();
	await importLink(page).click();
	await requireConfirmation(page, 'the library while its audio is playing');

	await stayButton(page).click();
	await expect(confirmation(page)).toHaveCount(0);
	await expect(page).toHaveURL(/\/library\?track=coral/);
	await expect(page.getByTestId('audio-player')).toHaveCount(1);
	expect(
		await liveElement!.evaluate(
			(el) => el.isConnected && el === document.querySelector('[data-testid="audio-player"]')
		),
		'expected the same native audio element after Stay. Builder: preserve element identity on the library route too.'
	).toBe(true);
	await expect(page.getByTestId('audio-player')).toHaveAttribute('src', srcBefore ?? '');
	expect(
		await page.getByTestId('audio-player').evaluate((el: HTMLAudioElement) => el.currentTime),
		'expected Stay to preserve the library playback position. Builder: no reseek or remount.'
	).toBe(42.5);
	await expect(page.getByTestId('audio-player')).toHaveJSProperty('paused', false);
	await expect(page.locator('[data-testid="queue-item"][aria-current="true"]')).toHaveAttribute(
		'data-track-id',
		'coral'
	);
	expect(
		await page.evaluate(() => {
			const probe = (window as unknown as ProbeWindow).__routeExit;
			return { playCalls: probe.playCalls, pauseCalls: probe.pauseCalls };
		}),
		'expected no extra play()/pause() calls across open+Stay on the library route. Builder: asking changes nothing.'
	).toEqual(callsBefore);
	await expect(
		importLink(page),
		'expected focus restored to the shell link that attempted the library exit. Builder: restore pre-dialog focus.'
	).toBeFocused();
});

test('[NS-L04] library playing: Leave exits exactly once and Back remounts fresh with no resumed audio', async ({
	page
}) => {
	await openLibraryStream(page);
	await startLibraryPlaying(page);
	const liveElement = await page.getByTestId('audio-player').elementHandle();

	await importLink(page).click();
	await requireConfirmation(page, 'the library while its audio is playing');
	await leaveButton(page).click();

	await expect(page).toHaveURL(/\/library\/import$/);
	await expect(page.getByTestId('import-file')).toBeVisible();
	await expect(leaveButton(page)).toHaveCount(0);
	expect(
		await liveElement!.evaluate((el) => el.isConnected),
		'expected the library audio element detached after Leave. Builder: normal route teardown, no persistent player.'
	).toBe(false);

	// Back remounts the library fresh. The share link is consumed once per
	// session (shipped behavior), so the remount shows the rows again with
	// no resumed queue, no audio and no prompt: Leave ended the session.
	await page.goBack();
	await expect(page).toHaveURL(/\/library\?track=coral/);
	await expect(page.getByTestId('library-item')).toHaveCount(LIBRARY_STREAM_ITEMS.length);
	await expect(
		page.getByTestId('audio-player'),
		'expected no resumed library audio after Back (Leave ended the session and the one-shot share link stays consumed). ' +
			'Builder: never silently restore playback after a Leave.'
	).toHaveCount(0);
	await expect(
		page.getByTestId('queue-item'),
		'expected no resumed queue after Back. Builder: Leave runs the ordinary teardown; nothing is restored.'
	).toHaveCount(0);
	await expect(leaveButton(page)).toHaveCount(0);
});

test('[NS-L05] library playing then native pause exits without a prompt (GREEN control)', async ({
	page
}) => {
	await openLibraryStream(page);
	await startLibraryPlaying(page);
	await pauseLibraryNative(page);

	await importLink(page).click();
	await expectUnpromptedExit(
		page,
		/\/library\/import$/,
		'the library after the listener paused the native audio'
	);
});

test('[NS-L06] library source change while playing: prompt protects the new source on Stay', async ({
	page
}) => {
	await openLibraryStream(page);
	await libraryRow(page, 'pine').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	await startLibraryPlaying(page);

	await page.getByTestId('player-next').click();
	await expect(page.locator('[data-testid="queue-item"][aria-current="true"]')).toHaveAttribute(
		'data-track-id',
		'pine'
	);
	await startLibraryPlaying(page);
	await tagAudioTime(page.getByTestId('audio-player'), 9.5);
	const pineSrc = await page.getByTestId('audio-player').getAttribute('src');
	expect(pineSrc).toContain('pine');

	await importLink(page).click();
	await requireConfirmation(page, 'the library after a source change into playing');
	await stayButton(page).click();

	await expect(page.getByTestId('audio-player')).toHaveAttribute('src', pineSrc ?? '');
	expect(
		await page.getByTestId('audio-player').evaluate((el: HTMLAudioElement) => el.currentTime),
		'expected Stay to preserve the new source position. Builder: follow the current actual audio across source swaps.'
	).toBe(9.5);
	await expect(page.getByTestId('audio-player')).toHaveJSProperty('paused', false);
	await expect(page.locator('[data-testid="queue-item"][aria-current="true"]')).toHaveAttribute(
		'data-track-id',
		'pine'
	);
});

test('[NS-L07] a cross-tab auth transition tears library playback down at once and never prompts (GREEN control)', async ({
	page
}) => {
	await openLibraryStream(page);
	await startLibraryPlaying(page);

	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate((key) => {
		window.localStorage.setItem(key, 'test-auth-transition');
		window.localStorage.removeItem(key);
	}, 'nntune.library.auth-transition.v1');
	await otherTab.close();

	await expect(
		page.getByRole('alert'),
		'expected the existing session-changed recovery to appear immediately, with no confirmation first. ' +
			'Builder: auth-transition teardown is never gated by the listening confirmation.'
	).toContainText(/session changed/i);
	await expect(
		page.getByTestId('queue-item'),
		'expected the previous session queue cleared by the transition. Builder: existing isolation wins over playback.'
	).toHaveCount(0);
	await expect(
		page.getByTestId('audio-player'),
		'expected the previous session audio removed by the transition. Builder: never hold teardown for a Leave/Stay answer.'
	).toHaveCount(0);
	await expect(leaveButton(page)).toHaveCount(0);
	await expect(stayButton(page)).toHaveCount(0);
});

test('[NS-L08] an expired session (401) reaches sign-in recovery without a listening prompt (GREEN control)', async ({
	page
}) => {
	let expired = false;
	await page.route('**/api/library', (route) =>
		expired
			? route.fulfill({
					status: 401,
					contentType: 'application/json',
					body: JSON.stringify({ error: 'unauthorized' })
				})
			: route.fulfill({ json: LIBRARY_STREAM_ITEMS })
	);
	await page.route('**/test-audio/**', () => {});
	await page.addInitScript(() => {
		HTMLMediaElement.prototype.play = function () {
			Object.defineProperty(this, 'paused', { value: false, configurable: true });
			return Promise.resolve();
		};
		HTMLMediaElement.prototype.pause = function () {
			Object.defineProperty(this, 'paused', { value: true, configurable: true });
		};
	});
	await page.goto('/library?track=coral&t=90');
	await expect(page.getByTestId('audio-player')).toHaveCount(1);
	await startLibraryPlaying(page);

	expired = true;
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	const signIn = page.getByRole('link', { name: /sign in/i });
	await expect(
		signIn,
		'expected the existing 401 sign-in recovery after refresh. Builder: expired-session handling is never delayed by this feature.'
	).toBeVisible();
	await expect(leaveButton(page)).toHaveCount(0);

	await signIn.click();
	await expect(
		page,
		'expected the sign-in recovery link to reach /login immediately, with no Leave/Stay prompt. ' +
			'Builder: credential/session exits bypass the confirmation entirely.'
	).toHaveURL(/\/login$/);
	await expect(leaveButton(page)).toHaveCount(0);
});

test('[NS-L09] library Back: Stay preserves native playback; once paused, Back simply leaves', async ({
	page
}) => {
	await page.route('**/api/library', (route) => route.fulfill({ json: LIBRARY_STREAM_ITEMS }));
	await page.route('**/test-audio/**', () => {});
	await page.addInitScript(() => {
		const probe = { playCalls: 0, pauseCalls: 0 };
		(window as unknown as ProbeWindow).__routeExit = probe;
		HTMLMediaElement.prototype.play = function () {
			probe.playCalls += 1;
			Object.defineProperty(this, 'paused', { value: false, configurable: true });
			return Promise.resolve();
		};
		HTMLMediaElement.prototype.pause = function () {
			probe.pauseCalls += 1;
			Object.defineProperty(this, 'paused', { value: true, configurable: true });
		};
	});
	await page.goto('/pricing');
	await anchorNavigate(page, '/library?track=coral&t=90');
	await expect(page.getByTestId('audio-player')).toHaveCount(1);
	await startLibraryPlaying(page);
	await tagAudioTime(page.getByTestId('audio-player'), 21.5);
	const liveElement = await page.getByTestId('audio-player').elementHandle();

	await page.getByRole('button', { name: 'Refresh', exact: true }).focus();
	await page.goBack();
	await requireConfirmation(page, 'the library via Back while playing');
	await stayButton(page).click();

	await expect(page).toHaveURL(/\/library\?track=coral/);
	expect(
		await liveElement!.evaluate(
			(el) => el.isConnected && el === document.querySelector('[data-testid="audio-player"]')
		),
		'expected the same library audio element after Back + Stay. Builder: a cancelled popstate keeps the session intact.'
	).toBe(true);
	expect(
		await page.getByTestId('audio-player').evaluate((el: HTMLAudioElement) => el.currentTime)
	).toBe(21.5);
	await expect(page.getByTestId('audio-player')).toHaveJSProperty('paused', false);
	await expect(
		page.getByRole('button', { name: 'Refresh', exact: true }),
		'expected focus restored to the control focused before Back. Builder: restore pre-dialog focus.'
	).toBeFocused();

	// After a genuine pause the same Back is an ordinary navigation.
	await pauseLibraryNative(page);
	await page.goBack();
	await expect(
		page,
		'expected Back from the paused library to reach the previous route with no prompt. ' +
			'Builder: Stay does not leave a sticky guard behind.'
	).toHaveURL(/\/pricing$/);
	await expect(leaveButton(page)).toHaveCount(0);
});

// ---------------------------------------------------------------------------
// Auth surface outside playback routes
// ---------------------------------------------------------------------------

test('[NS-A01] account sign-out reaches the signed-out route with no listening prompt (GREEN control)', async ({
	page
}) => {
	await page.route('**/v1/**', (route) => {
		const path = new URL(route.request().url()).pathname;
		return route.fulfill(
			path === '/v1/session/logout'
				? { status: 200, json: { ok: true } }
				: { status: 500, json: {} }
		);
	});
	await page.goto('/account/security');
	const signOut = page.getByRole('button', { name: 'Sign out', exact: true });
	await expect(signOut).toBeVisible();
	await expect(leaveButton(page)).toHaveCount(0);

	await signOut.click();
	await expect(
		page,
		'expected the mocked successful sign-out to reach /login immediately. ' +
			'Builder: credential/session mutations are never intercepted by a listening confirmation.'
	).toHaveURL(/\/login$/);
	await expect(leaveButton(page)).toHaveCount(0);
	await expect(stayButton(page)).toHaveCount(0);
});

// ---------------------------------------------------------------------------
// Presentation and side effects
// ---------------------------------------------------------------------------

for (const scheme of ['light', 'dark'] as const) {
	test(`[NS-P01] the confirmation fits a 390px ${scheme} viewport with both choices reachable`, async ({
		page
	}, testInfo) => {
		await page.emulateMedia({ colorScheme: scheme });
		await page.setViewportSize({ width: 390, height: 844 });
		await openDemo(page);
		await startDemoPlaying(page);

		await libraryLink(page).click();
		await requireConfirmation(page, `the demo player at 390px in ${scheme} mode`);
		await page.screenshot({ path: testInfo.outputPath(`route-exit-390-${scheme}.png`) });

		for (const [label, button] of [
			['Stay', stayButton(page)],
			['Leave and stop listening', leaveButton(page)]
		] as const) {
			await expect(button, `expected "${label}" visible at 390px (${scheme}).`).toBeVisible();
			const box = await button.boundingBox();
			expect(box, `expected "${label}" to have a layout box at 390px (${scheme}).`).not.toBeNull();
			if (box) {
				expect(
					box.x >= -1 && box.x + box.width <= 391,
					`expected "${label}" (x=${box.x}, w=${box.width}) inside the 390px ${scheme} viewport. ` +
						'Builder: the confirmation wraps on narrow screens in both themes.'
				).toBe(true);
				expect(
					box.height,
					`expected "${label}" to keep a usable touch height at 390px (${scheme}), got ${box.height}. ` +
						'Builder: keep controls at least 24px tall.'
				).toBeGreaterThanOrEqual(24);
			}
		}
		const overflow = await page.evaluate(() => ({
			scrollWidth: document.documentElement.scrollWidth,
			clientWidth: document.documentElement.clientWidth
		}));
		expect(
			overflow.scrollWidth <= overflow.clientWidth + 1,
			`expected no horizontal overflow with the dialog open at 390px (${scheme}); ` +
				`scrollWidth ${overflow.scrollWidth} vs clientWidth ${overflow.clientWidth}. Builder: constrain the dialog to the viewport.`
		).toBe(true);

		await stayButton(page).click();
		await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
		await page.screenshot({ path: testInfo.outputPath(`route-exit-stayed-390-${scheme}.png`) });
	});
}

test('[NS-P02] opening the confirmation and choosing Stay adds no storage keys and sends no mutating or off-origin requests', async ({
	page,
	context
}, testInfo) => {
	const originHost = '127.0.0.1:4173';
	const seen: string[] = [];
	const violations: string[] = [];
	context.on('request', (request) => {
		const entry = `${request.method()} ${request.url()}`;
		seen.push(entry);
	});
	await openDemo(page);
	await startDemoPlaying(page);

	const keysBefore = await page.evaluate(() => ({
		local: Object.keys(localStorage).sort(),
		session: Object.keys(sessionStorage).sort()
	}));
	const seenBefore = seen.length;
	const origin = new URL(page.url()).origin;
	void originHost;

	await libraryLink(page).click();
	await requireConfirmation(page, 'the demo player for the side-effect audit');
	await stayButton(page).click();
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');

	const during = seen.slice(seenBefore);
	for (const entry of during) {
		const [method, url] = entry.split(' ');
		if (method !== 'GET' && method !== 'HEAD' && method !== 'OPTIONS') violations.push(entry);
		if (!url.startsWith(`${origin}/`) && url !== origin) violations.push(entry);
	}
	await testInfo.attach('route-exit-network', {
		body: JSON.stringify({ during, violations }, null, 2),
		contentType: 'application/json'
	});
	expect(
		violations,
		`expected open+Stay to send no mutating or off-origin requests; saw ${JSON.stringify(violations)}. ` +
			'Builder: the confirmation is pure local UI (GET preloads of the attempted route are fine).'
	).toEqual([]);

	const keysAfter = await page.evaluate(() => ({
		local: Object.keys(localStorage).sort(),
		session: Object.keys(sessionStorage).sort()
	}));
	expect(
		keysAfter,
		'expected open+Stay to add no storage keys. Builder: no persistence for this warning.'
	).toEqual(keysBefore);
});
