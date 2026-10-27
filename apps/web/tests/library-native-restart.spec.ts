import { expect, test, type ElementHandle, type Page, type TestInfo } from '@playwright/test';

/**
 * LIBRESTART — restart the current native library track, tests-only handoff.
 *
 * PROTECTED ACCEPTANCE BANK: this file is the only protected file. The paired
 * builder implements the control on this branch and must not edit this file.
 * Builder-owned implementation surface: `src/routes/library/+page.svelte`
 * (plus, if needed, `src/lib/player/*` it already uses). Do not import the
 * demo player's Range-fetch transport or shortcut controller: `/library`
 * plays through the native `audio-player` element only.
 *
 * LOCKED CONTRACT (new hooks; every existing library/queue/share hook stays):
 * - `library-restart`: ONE native <button> inside the Player region (the
 *   region holding the native `audio[controls]`), beside the player, with
 *   accessible name matching /restart current track/i. Distinct from
 *   `player-next`; never a Previous-track action. Pointer, touch, Enter and
 *   Space all activate it; it is Tab-reachable with a visible focus ring.
 * - `library-restart-status`: ONE concise (<100 chars) polite live region
 *   (aria-live="polite" or role="status") inside the Player region. It is
 *   the only restart announcement path (no role="alert" restart outcome,
 *   no duplicate live path, no per-`timeupdate` chatter).
 * - Availability is LIVE native state of the currently mounted matching
 *   `audio-player` element, re-evaluated on media events
 *   (loadedmetadata/durationchange/canplay/progress/timeupdate): enabled
 *   only when `duration` is finite and > 0, `currentTime` is finite, and
 *   position 0 lies inside a `seekable` range. Unavailable, stale,
 *   nonfinite or unseekable state DISABLES the button and shows an
 *   understandable explanation as visible text in the Player region
 *   (not title-attribute only). Sound media at position 0 stays ENABLED.
 * - Activation writes `currentTime = 0` on that same element, inside a
 *   caught setter, and verifies by READBACK (live `currentTime` is 0,
 *   allowing <= 0.05s tolerance) before claiming success. Success status
 *   matches /restarted|back to (the )?start|from the start/i and names the
 *   current track. A throwing or silently-ignoring (sticky) setter yields
 *   a truthful failure status (/could not|unable|failed|unavailable/i),
 *   leaves the position untouched and never throws through the UI.
 *   Already-at-start activation (position 0) yields /already at (the )?start/i,
 *   never a restarted claim.
 * - A restart changes nothing else: paused/playing intent, `src`, the
 *   audio node itself (no reload/remount), queue order and current
 *   identity, `playbackRate` and `volume` are preserved; `play()`,
 *   `pause()` and `load()` are never called by the gesture; no track
 *   advance and no completion/ended accounting. NO global keyboard
 *   shortcut is added (the demo `0` binding does not exist here).
 * - An explicit restart SUPERSEDES any pending/remembered inbound-share
 *   seek for that source: later loadedmetadata/durationchange/canplay
 *   events for the same source must not move the position away from 0.
 *   A pending share seek for one track never applies to another track.
 *   Source changes, queue clear and auth transitions invalidate pending
 *   seeks and stale restart outcomes; events from a detached/stale audio
 *   element never redirect, disable or complete a restart.
 * - With no playable current track there is no enabled restart: the
 *   Player region (with the control) stays hidden as today, or the
 *   control is absent/disabled. Native controls stay intact. Layout
 *   holds at 320px in light and dark with no horizontal overflow and
 *   readable (>= 4.5:1) control text.
 *
 * Harness: the real `/library` route with `/api/library` fulfilled
 * in-test and `/test-audio/**` held pending, exactly like the library
 * share bank; per-element media doubles (duration/currentTime/seekable/
 * paused/readyState via `Object.defineProperty`) exactly like the demo
 * restart bank. No real decoder, no sleeps, no network. CTRL-01..03 are
 * baseline controls (GREEN before and after); LIBRESTART-LR15 is a guard
 * that is also GREEN on baseline by design. Every other LIBRESTART case
 * fails on baseline at an assertion naming the missing hook — never an
 * import/tooling failure (this file imports only `@playwright/test`).
 * Every case attaches a compact JSON receipt {case_id, expected, actual,
 * status, error_code, next_action}.
 *
 * Setup (cached packages/browser required for offline use):
 *   pnpm --dir apps/web install --offline --frozen-lockfile
 * Run (4173 must be free; CI=1 prevents reuse of another preview):
 *   CI=1 PUBLIC_API_BASE_URL='' pnpm --dir apps/web exec playwright test \
 *     tests/library-native-restart.spec.ts --workers=1 --retries=0
 * Regression: same command with tests/library-share-link.spec.ts,
 * tests/player-restart.spec.ts, tests/library-native-now-playing.spec.ts,
 * tests/library-player-queue.spec.ts. Plus `pnpm --dir apps/web check`
 * and the full `pnpm --dir apps/web test` Vitest suite.
 */

test.setTimeout(30_000);
test.use({ viewport: { width: 1000, height: 740 } });

type Item = { id: string; title: string; artist?: string; streamUrl?: string };
type Calls = { play: number; pause: number; load: number };

const ITEMS: Item[] = [
	{ id: 'lr-coral', title: 'Coral Night', artist: 'Mira', streamUrl: '/test-audio/lr-coral' },
	{ id: 'lr-pine', title: 'Pine Morning', artist: 'North', streamUrl: '/test-audio/lr-pine' },
	{ id: 'lr-ember', title: 'Ember Line', streamUrl: '/test-audio/lr-ember' }
];
const NEXT_ACCOUNT: Item[] = [
	{ id: 'lr-bravo', title: 'Bravo Song', artist: 'Next Account', streamUrl: '/test-audio/lr-bravo' }
];
const PLAIN_ITEMS: Item[] = [{ id: 'lr-plain', title: 'Plain Coral', artist: 'Mira' }];

const SUCCESS = /restarted|back to (the )?start|from the start/i;
const ALREADY = /already at (the )?start/i;
const FAILURE = /could not|unable|failed|unavailable/i;
const EXPLANATION = /unavailable|not available|cannot|can't|unable|waiting|loading|metadata|seekable|not ready/i;

const playerRegion = (page: Page) => page.getByRole('region', { name: 'Player', exact: true });
const audio = (page: Page) => page.getByTestId('audio-player');
const restartButton = (page: Page) => page.getByTestId('library-restart');
const restartStatus = (page: Page) => page.getByTestId('library-restart-status');
const row = (page: Page, id: string) => page.locator(`[data-testid="library-item"][data-id="${id}"]`);

async function openLibrary(
	page: Page,
	options: { url?: string; items?: Item[]; payload?: { current: Item[] } } = {}
) {
	const payload = options.payload ?? { current: options.items ?? ITEMS };
	await page.route('**/api/library', (route) =>
		route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(payload.current) })
	);
	await page.route('**/test-audio/**', () => {});
	await page.addInitScript(() => {
		const calls: Calls = { play: 0, pause: 0, load: 0 };
		(window as unknown as { __lrCalls: Calls }).__lrCalls = calls;
		HTMLMediaElement.prototype.play = function () {
			calls.play += 1;
			return new Promise<void>(() => {});
		};
		HTMLMediaElement.prototype.pause = function () {
			calls.pause += 1;
		};
		const rawLoad = HTMLMediaElement.prototype.load;
		HTMLMediaElement.prototype.load = function () {
			calls.load += 1;
			return rawLoad.call(this);
		};
	});
	await page.goto(options.url ?? '/library');
	await expect(page.getByTestId('library-item')).toHaveCount(payload.current.length);
	return payload;
}

/** Select a row via its select control (name STARTS with the title). */
async function selectRow(page: Page, id: string, title: string) {
	const escaped = title.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
	await row(page, id)
		.getByRole('button', { name: new RegExp(`^${escaped}( — |$)`) })
		.click();
}

async function settle(page: Page) {
	await page.evaluate(
		() => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())))
	);
}

async function callsOf(page: Page): Promise<Calls> {
	return page.evaluate(() => ({ ...(window as unknown as { __lrCalls: Calls }).__lrCalls }));
}

async function queueOrder(page: Page) {
	return page
		.getByTestId('queue-item')
		.evaluateAll((els) => els.map((el) => el.getAttribute('data-track-id')));
}

async function currentQueueId(page: Page): Promise<string | null> {
	const ids = await page
		.locator('[data-testid="queue-item"][aria-current="true"]')
		.evaluateAll((els) => els.map((el) => el.getAttribute('data-track-id')));
	return ids[0] ?? null;
}

type MockSeed = {
	time: number;
	duration: number;
	ranges?: [number, number][];
	paused?: boolean;
	readyState?: number;
};

/** Deterministic media double on the CURRENT audio element. */
async function mockMedia(page: Page, seed: MockSeed) {
	await audio(page).evaluate((el: HTMLAudioElement, s: Required<MockSeed>) => {
		let time = s.time;
		Object.defineProperty(el, 'duration', { value: s.duration, configurable: true });
		Object.defineProperty(el, 'currentTime', {
			get: () => time,
			set: (next: number) => {
				time = next;
			},
			configurable: true
		});
		Object.defineProperty(el, 'seekable', {
			get: () => ({
				length: s.ranges.length,
				start: (i: number) => s.ranges[i][0],
				end: (i: number) => s.ranges[i][1]
			}),
			configurable: true
		});
		Object.defineProperty(el, 'paused', { value: s.paused, configurable: true });
		Object.defineProperty(el, 'readyState', { value: s.readyState, configurable: true });
	}, {
		time: seed.time,
		duration: seed.duration,
		ranges: seed.ranges ?? [[0, seed.duration]],
		paused: seed.paused ?? true,
		readyState: seed.readyState ?? 4
	});
}

/** Fire the native events any live-availability listener may key on. */
async function signalMedia(page: Page) {
	await audio(page).evaluate((el: HTMLAudioElement) => {
		for (const name of ['loadedmetadata', 'durationchange', 'canplay', 'progress', 'timeupdate']) {
			el.dispatchEvent(new Event(name));
		}
	});
	await settle(page);
}

async function timeOf(page: Page): Promise<number> {
	return audio(page).evaluate((el: HTMLAudioElement) => el.currentTime);
}

const timeRepr = (value: number) => (Number.isNaN(value) ? 'NaN' : value);

async function setTime(page: Page, seconds: number) {
	await audio(page).evaluate((el: HTMLAudioElement, value: number) => {
		el.currentTime = value;
		el.dispatchEvent(new Event('timeupdate'));
	}, seconds);
	await settle(page);
}

async function hookState(page: Page): Promise<'missing' | 'disabled' | 'enabled'> {
	const button = restartButton(page);
	if ((await button.count()) === 0) return 'missing';
	return (await button.isDisabled()) ? 'disabled' : 'enabled';
}

async function requireHook(page: Page, caseId: string) {
	const state = await hookState(page);
	expect(
		state,
		JSON.stringify({
			case_id: caseId,
			expected: 'a visible Restart current track button (data-testid="library-restart") in the Player region',
			actual: state,
			status: 'RED',
			error_code: 'LIBRARY_RESTART_MISSING',
			next_action: 'render the native library-restart button beside the library native player; do not edit this file'
		})
	).not.toBe('missing');
	return state;
}

async function statusText(page: Page): Promise<string> {
	if ((await restartStatus(page).count()) !== 1) return '';
	return ((await restartStatus(page).textContent()) ?? '').replace(/\s+/g, ' ').trim();
}

async function outcome(page: Page, trackTitle: string) {
	const text = await statusText(page);
	return {
		count: await restartStatus(page).count(),
		inPlayer: (await playerRegion(page).getByTestId('library-restart-status').count()) === 1,
		success: SUCCESS.test(text) && !ALREADY.test(text),
		already: ALREADY.test(text),
		failure: FAILURE.test(text) && !SUCCESS.test(text),
		namesTrack: text.includes(trackTitle),
		concise: text.length > 0 && text.length < 100,
		polite:
			(await restartStatus(page).count()) === 1 &&
			(await restartStatus(page).evaluate(
				(el) => el.getAttribute('aria-live') === 'polite' || el.getAttribute('role') === 'status'
			))
	};
}

const okOutcome = {
	count: 1,
	inPlayer: true,
	success: true,
	already: false,
	failure: false,
	namesTrack: true,
	concise: true,
	polite: true
};

async function explanationVisible(page: Page): Promise<boolean> {
	if ((await playerRegion(page).count()) !== 1) return false;
	const text = await playerRegion(page).innerText().catch(() => '');
	return EXPLANATION.test(text);
}

async function check(testInfo: TestInfo, expected: unknown, actual: unknown) {
	let passed = false;
	try {
		expect(actual).toEqual(expected);
		passed = true;
	} finally {
		const receipt = {
			case_id: testInfo.title.split(' ')[0],
			expected,
			actual,
			status: passed ? 'GREEN' : 'RED',
			error_code: passed ? null : 'LIBRARY_RESTART_CONTRACT',
			next_action: passed
				? 'preserve acceptance'
				: 'implement the library restart contract in the library page; do not edit this file'
		};
		const body = JSON.stringify(receipt);
		console.log(body);
		await testInfo.attach('acceptance-case.json', { body, contentType: 'application/json' });
	}
}

test.afterEach(async ({}, testInfo) => {
	if (testInfo.attachments.some((attachment) => attachment.name === 'acceptance-case.json')) return;
	const errors = testInfo.errors.map((error) => error.message ?? String(error));
	const missingHook = errors.some((message) => message.includes('LIBRARY_RESTART_MISSING'));
	const body = JSON.stringify({
		case_id: testInfo.title.split(' ')[0],
		expected: 'reach behavioral assertion',
		actual: errors,
		status: missingHook ? 'RED' : 'ERROR',
		error_code: missingHook ? 'LIBRARY_RESTART_MISSING' : 'HARNESS_OR_ACTION_FAILURE',
		next_action: missingHook
			? 'render the native library-restart button beside the library native player; do not edit this file'
			: 'repair harness/action; not verified feature RED'
	});
	console.log(body);
	await testInfo.attach('acceptance-case.json', { body, contentType: 'application/json' });
});

/** Select coral and prove the native player mounted, as every case needs. */
async function mountCoral(page: Page) {
	await selectRow(page, 'lr-coral', 'Coral Night');
	await expect(audio(page)).toHaveCount(1);
	await expect(page.getByTestId('now-playing')).toContainText('Coral Night');
}

test('CTRL-01 baseline library selection mounts one controlled native audio in the Player region', async ({ page }, info) => {
	await openLibrary(page);
	await mountCoral(page);
	await check(info, { region: 1, audioInRegion: 1, controls: true, src: '/test-audio/lr-coral', current: 'lr-coral' }, {
		region: await playerRegion(page).count(),
		audioInRegion: await playerRegion(page).locator('audio[controls]').count(),
		controls: await audio(page).evaluate((el: HTMLAudioElement) => el.hasAttribute('controls')),
		src: await audio(page).getAttribute('src'),
		current: await currentQueueId(page)
	});
});

test('CTRL-02 the per-element media double reads and writes deterministically', async ({ page }, info) => {
	await openLibrary(page);
	await mountCoral(page);
	await mockMedia(page, { time: 30, duration: 120 });
	const seeded = await timeOf(page);
	await setTime(page, 42.5);
	await check(info, { seeded: 30, written: 42.5, duration: 120, seekableEnd: 120 }, {
		seeded,
		written: await timeOf(page),
		duration: await audio(page).evaluate((el: HTMLAudioElement) => el.duration),
		seekableEnd: await audio(page).evaluate((el: HTMLAudioElement) => el.seekable.end(0))
	});
});

test('CTRL-03 baseline inbound share seek lands on the library audio element', async ({ page }, info) => {
	await openLibrary(page, { url: '/library?track=lr-coral&t=77' });
	await expect(page.getByTestId('now-playing')).toContainText('Coral Night');
	await expect(audio(page)).toHaveCount(1);
	await expect.poll(() => timeOf(page)).toBe(77);
	await check(info, { current: 'lr-coral', time: 77, playCalls: 0 }, {
		current: await currentQueueId(page),
		time: await timeOf(page),
		playCalls: (await callsOf(page)).play
	});
});

test('LIBRESTART-LR01 a labeled native Restart current track button lives in the Player region', async ({ page }, info) => {
	await openLibrary(page);
	await mountCoral(page);
	await requireHook(page, 'LIBRESTART-LR01');
	const button = restartButton(page);
	const nameOf = async (locator: ReturnType<Page['getByTestId']>) =>
		((await locator.evaluate((el) => el.getAttribute('aria-label') ?? el.textContent ?? '')) as string)
			.replace(/\s+/g, ' ')
			.trim();
	const restartName = await nameOf(button);
	const nextName = await nameOf(page.getByTestId('player-next'));
	await check(info, {
		count: 1,
		inPlayer: true,
		isButton: true,
		nameOk: true,
		distinctFromNext: true,
		audioControls: true,
		preStatusClean: true
	}, {
		count: await button.count(),
		inPlayer: (await playerRegion(page).getByTestId('library-restart').count()) === 1,
		isButton: (await button.evaluate((el) => el.tagName)) === 'BUTTON',
		nameOk: /restart current track/i.test(restartName),
		distinctFromNext:
			restartName.toLowerCase() !== nextName.toLowerCase() && !/restart/i.test(nextName),
		audioControls: await audio(page).evaluate((el: HTMLAudioElement) => el.hasAttribute('controls')),
		preStatusClean: !SUCCESS.test(await statusText(page))
	});
	await expect(button).toHaveAccessibleName(/restart current track/i);
	await expect(button).not.toHaveAccessibleName(/previous/i);
});

test('LIBRESTART-LR02 keyboard reaches the button with a visible ring; Enter and Space restart', async ({ page }, info) => {
	await openLibrary(page);
	await mountCoral(page);
	await mockMedia(page, { time: 60, duration: 120 });
	await signalMedia(page);
	await requireHook(page, 'LIBRESTART-LR02');

	await page.evaluate(() => {
		if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
	});
	let focused: string | null = null;
	for (let tab = 0; tab < 80; tab += 1) {
		await page.keyboard.press('Tab');
		focused = await page.evaluate(
			() => (document.activeElement as HTMLElement | null)?.dataset?.testid ?? null
		);
		if (focused === 'library-restart') break;
	}
	const ring = await page.evaluate(() => {
		const el = document.activeElement as HTMLElement | null;
		if (!el || el.dataset.testid !== 'library-restart') return { visible: false };
		const style = getComputedStyle(el);
		return { visible: (style.outlineStyle !== 'none' && style.outlineWidth !== '0px') || style.boxShadow !== 'none' };
	});
	const beforeEnter = await callsOf(page);
	await page.keyboard.press('Enter');
	await expect.poll(() => timeOf(page)).toBe(0);
	await setTime(page, 45);
	await restartButton(page).focus();
	await page.keyboard.press('Space');
	await expect.poll(() => timeOf(page)).toBe(0);

	await check(info, {
		tabReached: true,
		ringVisible: true,
		afterSpace: 0,
		extraCalls: { play: 0, pause: 0, load: 0 },
		outcome: okOutcome
	}, {
		tabReached: focused === 'library-restart',
		ringVisible: ring.visible,
		afterSpace: await timeOf(page),
		extraCalls: await callsOf(page).then((after) => ({
			play: after.play - beforeEnter.play,
			pause: after.pause - beforeEnter.pause,
			load: after.load - beforeEnter.load
		})),
		outcome: await outcome(page, 'Coral Night')
	});
});

test('LIBRESTART-LR03 pointer restart while paused preserves intent src queue rate volume and node', async ({ page }, info) => {
	await openLibrary(page);
	await mountCoral(page);
	await selectRow(page, 'lr-pine', 'Pine Morning');
	await mockMedia(page, { time: 30, duration: 120, paused: true });
	await audio(page).evaluate((el: HTMLAudioElement & { __lrMarker?: string }) => {
		el.__lrMarker = 'same-node';
		el.playbackRate = 1.5;
		el.volume = 0.4;
	});
	await signalMedia(page);
	await requireHook(page, 'LIBRESTART-LR03');
	const before = { calls: await callsOf(page), queue: await queueOrder(page) };

	await restartButton(page).click();
	await expect.poll(() => timeOf(page)).toBe(0);

	await check(info, {
		time: 0,
		paused: true,
		src: '/test-audio/lr-coral',
		sameNode: true,
		queue: before.queue,
		current: 'lr-coral',
		rate: 1.5,
		volume: 0.4,
		extraCalls: { play: 0, pause: 0, load: 0 },
		audioControls: true,
		outcome: okOutcome
	}, {
		time: await timeOf(page),
		paused: await audio(page).evaluate((el: HTMLAudioElement) => el.paused),
		src: await audio(page).getAttribute('src'),
		sameNode: await audio(page).evaluate(
			(el: HTMLAudioElement & { __lrMarker?: string }) => el.__lrMarker === 'same-node'
		),
		queue: await queueOrder(page),
		current: await currentQueueId(page),
		rate: await audio(page).evaluate((el: HTMLAudioElement) => el.playbackRate),
		volume: await audio(page).evaluate((el: HTMLAudioElement) => el.volume),
		extraCalls: await callsOf(page).then((after) => ({
			play: after.play - before.calls.play,
			pause: after.pause - before.calls.pause,
			load: after.load - before.calls.load
		})),
		audioControls: await audio(page).evaluate((el: HTMLAudioElement) => el.hasAttribute('controls')),
		outcome: await outcome(page, 'Coral Night')
	});
	await expect(page.getByTestId('now-playing')).toContainText('Coral Night');
});

test('LIBRESTART-LR04 pointer restart while playing preserves playing intent without transport calls', async ({ page }, info) => {
	await openLibrary(page);
	await mountCoral(page);
	await mockMedia(page, { time: 45, duration: 120, paused: false });
	await audio(page).evaluate((el: HTMLAudioElement) => {
		el.dispatchEvent(new Event('play'));
		el.dispatchEvent(new Event('playing'));
	});
	await signalMedia(page);
	await requireHook(page, 'LIBRESTART-LR04');
	const before = await callsOf(page);

	await restartButton(page).click();
	await expect.poll(() => timeOf(page)).toBe(0);

	await check(info, {
		time: 0,
		paused: false,
		extraCalls: { play: 0, pause: 0, load: 0 },
		outcome: okOutcome
	}, {
		time: await timeOf(page),
		paused: await audio(page).evaluate((el: HTMLAudioElement) => el.paused),
		extraCalls: await callsOf(page).then((after) => ({
			play: after.play - before.play,
			pause: after.pause - before.pause,
			load: after.load - before.load
		})),
		outcome: await outcome(page, 'Coral Night')
	});
});

test.describe('touch', () => {
	test.use({ viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true });

	test('LIBRESTART-LR05 touch tap restarts the current track', async ({ page }, info) => {
		await openLibrary(page);
		await mountCoral(page);
		await mockMedia(page, { time: 33, duration: 120 });
		await signalMedia(page);
		await requireHook(page, 'LIBRESTART-LR05');
		const before = await callsOf(page);

		await restartButton(page).tap();
		await expect.poll(() => timeOf(page)).toBe(0);

		await check(info, { time: 0, extraCalls: { play: 0, pause: 0, load: 0 }, outcome: okOutcome }, {
			time: await timeOf(page),
			extraCalls: await callsOf(page).then((after) => ({
				play: after.play - before.play,
				pause: after.pause - before.pause,
				load: after.load - before.load
			})),
			outcome: await outcome(page, 'Coral Night')
		});
	});
});

test('LIBRESTART-LR06 restart at zero is a truthful already-at-start no-op', async ({ page }, info) => {
	await openLibrary(page);
	await mountCoral(page);
	await mockMedia(page, { time: 0, duration: 120 });
	await signalMedia(page);
	const state = await requireHook(page, 'LIBRESTART-LR06');
	const before = await callsOf(page);

	if (state === 'enabled') await restartButton(page).click();
	await settle(page);

	const result = await outcome(page, 'Coral Night');
	await check(info, {
		state: 'enabled',
		time: 0,
		already: true,
		success: false,
		concise: true,
		extraCalls: { play: 0, pause: 0, load: 0 }
	}, {
		state,
		time: await timeOf(page),
		already: result.already,
		success: result.success,
		concise: result.concise,
		extraCalls: await callsOf(page).then((after) => ({
			play: after.play - before.play,
			pause: after.pause - before.pause,
			load: after.load - before.load
		}))
	});
});

test('LIBRESTART-LR07 nonfinite or unseekable media disables with an explanation, then enables live', async ({ page }, info) => {
	await openLibrary(page);
	await mountCoral(page);
	await mockMedia(page, { time: 30, duration: 120 });
	await requireHook(page, 'LIBRESTART-LR07');

	const invalid: { seed: MockSeed; at: number }[] = [
		{ seed: { time: 30, duration: Number.NaN, ranges: [], readyState: 0 }, at: 30 },
		{ seed: { time: 30, duration: Number.POSITIVE_INFINITY, ranges: [[0, 120]] }, at: 30 },
		{ seed: { time: 0, duration: 0, ranges: [] }, at: 0 },
		{ seed: { time: 30, duration: 120, ranges: [] }, at: 30 },
		{ seed: { time: 30, duration: 120, ranges: [[10, 120]] }, at: 30 },
		{ seed: { time: Number.NaN, duration: 120, ranges: [[0, 120]] }, at: Number.NaN }
	];
	const seen = [];
	for (const { seed, at } of invalid) {
		await mockMedia(page, seed);
		await signalMedia(page);
		seen.push({
			state: await hookState(page),
			explanation: await explanationVisible(page),
			time: timeRepr(await timeOf(page)),
			at: timeRepr(at)
		});
	}

	await mockMedia(page, { time: 30, duration: 120 });
	await signalMedia(page);
	const liveState = await hookState(page);
	if (liveState === 'enabled') await restartButton(page).click();
	await settle(page);

	await check(info, {
		invalid: invalid.map(({ at }) => ({ state: 'disabled', explanation: true, time: timeRepr(at), at: timeRepr(at) })),
		liveState: 'enabled',
		afterRestart: 0,
		outcome: okOutcome
	}, {
		invalid: seen,
		liveState,
		afterRestart: await timeOf(page),
		outcome: await outcome(page, 'Coral Night')
	});
});

test('LIBRESTART-LR08 a throwing seek setter is caught with a truthful failure outcome', async ({ page }, info) => {
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(String(error)));
	await openLibrary(page);
	await mountCoral(page);
	await audio(page).evaluate((el: HTMLAudioElement) => {
		Object.defineProperty(el, 'duration', { value: 120, configurable: true });
		Object.defineProperty(el, 'currentTime', {
			get: () => 30,
			set: () => {
				throw new DOMException('seek refused by media', 'InvalidStateError');
			},
			configurable: true
		});
		Object.defineProperty(el, 'seekable', {
			get: () => ({ length: 1, start: () => 0, end: () => 120 }),
			configurable: true
		});
		Object.defineProperty(el, 'readyState', { value: 4, configurable: true });
	});
	await signalMedia(page);
	const state = await requireHook(page, 'LIBRESTART-LR08');
	const before = await callsOf(page);

	if (state === 'enabled') await restartButton(page).click();
	await settle(page);

	const result = await outcome(page, 'Coral Night');
	await check(info, {
		state: 'enabled',
		time: 30,
		failure: true,
		success: false,
		pageErrors: 0,
		extraCalls: { play: 0, pause: 0, load: 0 }
	}, {
		state,
		time: await timeOf(page),
		failure: result.failure,
		success: result.success,
		pageErrors: errors.length,
		extraCalls: await callsOf(page).then((after) => ({
			play: after.play - before.play,
			pause: after.pause - before.pause,
			load: after.load - before.load
		}))
	});
});

test('LIBRESTART-LR09 a silently ignoring seek setter never reports success from the attempted write', async ({ page }, info) => {
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(String(error)));
	await openLibrary(page);
	await mountCoral(page);
	await audio(page).evaluate((el: HTMLAudioElement) => {
		Object.defineProperty(el, 'duration', { value: 120, configurable: true });
		Object.defineProperty(el, 'currentTime', {
			get: () => 30,
			set: () => {},
			configurable: true
		});
		Object.defineProperty(el, 'seekable', {
			get: () => ({ length: 1, start: () => 0, end: () => 120 }),
			configurable: true
		});
		Object.defineProperty(el, 'readyState', { value: 4, configurable: true });
	});
	await signalMedia(page);
	const state = await requireHook(page, 'LIBRESTART-LR09');

	if (state === 'enabled') await restartButton(page).click();
	await settle(page);

	const result = await outcome(page, 'Coral Night');
	await check(info, {
		state: 'enabled',
		time: 30,
		failure: true,
		success: false,
		pageErrors: 0
	}, {
		state,
		time: await timeOf(page),
		failure: result.failure,
		success: result.success,
		pageErrors: errors.length
	});
});

test('LIBRESTART-LR10 restart follows the current element and source after Next; stale events are ignored', async ({ page }, info) => {
	await openLibrary(page);
	await mountCoral(page);
	await selectRow(page, 'lr-pine', 'Pine Morning');
	await mockMedia(page, { time: 30, duration: 120 });
	await signalMedia(page);
	await requireHook(page, 'LIBRESTART-LR10');
	const stale = (await audio(page).elementHandle()) as ElementHandle<HTMLAudioElement> | null;
	expect(stale, 'LIBRESTART-LR10: expected a library audio element before Next').not.toBeNull();
	if (!stale) return;
	await stale.evaluate((el: HTMLAudioElement & { __lrMarker?: string }) => {
		el.__lrMarker = 'pre-next';
	});

	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Pine Morning');
	const replaced = !(await audio(page).evaluate(
		(el: HTMLAudioElement & { __lrMarker?: string }) => el.__lrMarker === 'pre-next'
	));

	await mockMedia(page, { time: 50, duration: 200 });
	await signalMedia(page);
	if (replaced) {
		await stale.evaluate((el: HTMLAudioElement) => {
			el.dispatchEvent(new Event('loadedmetadata'));
			el.dispatchEvent(new Event('durationchange'));
		});
		await settle(page);
	}
	const stateAfterStale = await hookState(page);
	if (stateAfterStale === 'enabled') await restartButton(page).click();
	await settle(page);

	await check(info, {
		current: 'lr-pine',
		src: '/test-audio/lr-pine',
		stateAfterStale: 'enabled',
		time: 0,
		staleTimeUntouched: true,
		outcome: okOutcome
	}, {
		current: await currentQueueId(page),
		src: await audio(page).getAttribute('src'),
		stateAfterStale,
		time: await timeOf(page),
		staleTimeUntouched: replaced
			? (await stale.evaluate((el: HTMLAudioElement) => el.currentTime)) === 30
			: true,
		outcome: await outcome(page, 'Pine Morning')
	});
});

test('LIBRESTART-LR11 an explicit restart supersedes a pending inbound-share seek for that source', async ({ page }, info) => {
	await openLibrary(page, { url: '/library?track=lr-coral&t=77' });
	await expect(page.getByTestId('now-playing')).toContainText('Coral Night');
	await expect(audio(page)).toHaveCount(1);
	await expect.poll(() => timeOf(page)).toBe(77);

	await mockMedia(page, { time: 77, duration: 120 });
	await signalMedia(page);
	await requireHook(page, 'LIBRESTART-LR11');
	await restartButton(page).click();
	await expect.poll(() => timeOf(page)).toBe(0);

	// Later metadata for the SAME source must not undo the gesture.
	await audio(page).evaluate((el: HTMLAudioElement) => {
		for (const name of ['loadedmetadata', 'durationchange', 'canplay', 'canplaythrough']) {
			el.dispatchEvent(new Event(name));
		}
	});
	await settle(page);

	await check(info, { time: 0, current: 'lr-coral', outcome: okOutcome }, {
		time: await timeOf(page),
		current: await currentQueueId(page),
		outcome: await outcome(page, 'Coral Night')
	});
});

test('LIBRESTART-LR12 a share seek for one track never lands on the next source', async ({ page }, info) => {
	await openLibrary(page, { url: '/library?track=lr-coral&t=77' });
	await expect(page.getByTestId('now-playing')).toContainText('Coral Night');
	await expect.poll(() => timeOf(page)).toBe(77);
	await mockMedia(page, { time: 77, duration: 120 });

	await selectRow(page, 'lr-pine', 'Pine Morning');
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Pine Morning');
	await mockMedia(page, { time: 12, duration: 200 });
	await signalMedia(page);
	const beforeRestart = await timeOf(page);

	await requireHook(page, 'LIBRESTART-LR12');
	await restartButton(page).click();
	await expect.poll(() => timeOf(page)).toBe(0);

	await check(info, { beforeRestart: 12, time: 0, current: 'lr-pine', outcome: okOutcome }, {
		beforeRestart,
		time: await timeOf(page),
		current: await currentQueueId(page),
		outcome: await outcome(page, 'Pine Morning')
	});
});

test('LIBRESTART-LR13 auth teardown clears restart and pending state; the next account starts clean', async ({ page }, info) => {
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(String(error)));
	const payload = await openLibrary(page, { payload: { current: ITEMS } });
	await mountCoral(page);
	await mockMedia(page, { time: 40, duration: 120 });
	await signalMedia(page);
	await requireHook(page, 'LIBRESTART-LR13');
	await restartButton(page).click();
	await expect.poll(() => timeOf(page)).toBe(0);
	const stale = (await audio(page).elementHandle()) as ElementHandle<HTMLAudioElement> | null;

	// Same cross-tab auth transition the library queue bank uses.
	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate((key) => {
		window.localStorage.setItem(key, 'test-auth-transition');
		window.localStorage.removeItem(key);
	}, 'nntune.library.auth-transition.v1');
	await otherTab.close();
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	const tornDownAudio = await audio(page).count();
	const tornDownState = await hookState(page);
	if (stale) {
		await stale.evaluate((el: HTMLAudioElement) => {
			el.dispatchEvent(new Event('loadedmetadata'));
			el.dispatchEvent(new Event('canplay'));
		}).catch(() => {});
		await settle(page);
	}

	payload.current = NEXT_ACCOUNT;
	await page.getByRole('button', { name: 'Retry' }).click();
	await expect(page.getByTestId('library-list')).toContainText('Bravo Song');
	const staleStatus = await statusText(page);
	await selectRow(page, 'lr-bravo', 'Bravo Song');
	await expect(audio(page)).toHaveCount(1);
	const freshTime = await audio(page).evaluate((el: HTMLAudioElement) => {
		el.dispatchEvent(new Event('loadedmetadata'));
		return el.currentTime;
	});
	await mockMedia(page, { time: 20, duration: 90 });
	await signalMedia(page);
	await restartButton(page).click();
	await expect.poll(() => timeOf(page)).toBe(0);

	await check(info, {
		tornDownAudio: 0,
		tornDownEnabled: false,
		staleClaim: false,
		freshTime: 0,
		time: 0,
		pageErrors: 0,
		outcome: okOutcome
	}, {
		tornDownAudio,
		tornDownEnabled: tornDownState === 'enabled',
		staleClaim: /Coral Night/.test(staleStatus) && SUCCESS.test(staleStatus),
		freshTime,
		time: await timeOf(page),
		pageErrors: errors.length,
		outcome: await outcome(page, 'Bravo Song')
	});
});

test('LIBRESTART-LR14 no global shortcut restarts the library track', async ({ page }, info) => {
	await openLibrary(page);
	await mountCoral(page);
	await mockMedia(page, { time: 30, duration: 120 });
	await signalMedia(page);
	await requireHook(page, 'LIBRESTART-LR14');
	const before = await callsOf(page);

	await page.evaluate(() => {
		if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
	});
	for (const key of ['0', 'r', 'R']) await page.keyboard.press(key);
	await settle(page);

	await check(info, {
		time: 30,
		success: false,
		extraCalls: { play: 0, pause: 0, load: 0 }
	}, {
		time: await timeOf(page),
		success: (await outcome(page, 'Coral Night')).success,
		extraCalls: await callsOf(page).then((after) => ({
			play: after.play - before.play,
			pause: after.pause - before.pause,
			load: after.load - before.load
		}))
	});
});

test('LIBRESTART-LR15 guard: no playable current track never exposes an enabled restart', async ({ page }, info) => {
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(String(error)));
	await openLibrary(page);
	await mountCoral(page);
	await mockMedia(page, { time: 30, duration: 120 });
	await page.getByRole('button', { name: 'Clear queue', exact: true }).click();
	await settle(page);
	const cleared = {
		audio: await audio(page).count(),
		enabled: (await hookState(page)) === 'enabled'
	};

	await openLibrary(page, { items: PLAIN_ITEMS });
	await selectRow(page, 'lr-plain', 'Plain Coral');
	await settle(page);

	await check(info, {
		cleared: { audio: cleared.audio, enabled: false },
		streamlessAudio: 0,
		streamlessEnabled: false,
		pageErrors: 0
	}, {
		cleared,
		streamlessAudio: await audio(page).count(),
		streamlessEnabled: (await hookState(page)) === 'enabled',
		pageErrors: errors.length
	});
});

for (const [index, theme] of ['light', 'dark'].entries()) {
	test(`LIBRESTART-LR${16 + index} ${theme} 320px player keeps restart contained readable and working`, async ({ page }, info) => {
		await page.setViewportSize({ width: 320, height: 740 });
		await page.emulateMedia({ colorScheme: theme as 'light' | 'dark' });
		await openLibrary(page);
		await page.evaluate((value) => document.documentElement.setAttribute('data-appearance', value), theme);
		await mountCoral(page);
		await mockMedia(page, { time: 25, duration: 120 });
		await signalMedia(page);
		await requireHook(page, `LIBRESTART-LR${16 + index}`);

		const layout = await restartButton(page).evaluate((node) => {
			const box = node.getBoundingClientRect();
			const rgb = (value: string) => value.match(/[\d.]+/g)?.map(Number) ?? [];
			const luminance = (color: number[]) =>
				color
					.slice(0, 3)
					.map((n) => {
						const c = n / 255;
						return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
					})
					.reduce((sum, value, i) => sum + value * [0.2126, 0.7152, 0.0722][i], 0);
			let background: number[] = [];
			for (let element: Element | null = node; element; element = element.parentElement) {
				const value = rgb(getComputedStyle(element).backgroundColor);
				if (value.length === 3 || value[3] === 1) {
					background = value;
					break;
				}
			}
			const foreground = rgb(getComputedStyle(node).color);
			const values = [luminance(foreground), luminance(background)].sort((a, b) => b - a);
			return {
				contained:
					box.x >= 0 && box.right <= innerWidth + 1 && document.documentElement.scrollWidth <= innerWidth,
				readable: background.length >= 3 && (values[0] + 0.05) / (values[1] + 0.05) >= 4.5
			};
		});
		await restartButton(page).click();
		await expect.poll(() => timeOf(page)).toBe(0);

		await check(info, { layout: { contained: true, readable: true }, time: 0 }, {
			layout,
			time: await timeOf(page)
		});
	});
}

test('LIBRESTART-LR18 a near-end restart does not advance or count a completion', async ({ page }, info) => {
	await openLibrary(page);
	await mountCoral(page);
	await selectRow(page, 'lr-pine', 'Pine Morning');
	await mockMedia(page, { time: 119.9, duration: 120 });
	await signalMedia(page);
	await requireHook(page, 'LIBRESTART-LR18');
	const queue = await queueOrder(page);

	await restartButton(page).click();
	await expect.poll(() => timeOf(page)).toBe(0);
	await settle(page);

	await check(info, { time: 0, queue, current: 'lr-coral', outcome: okOutcome }, {
		time: await timeOf(page),
		queue: await queueOrder(page),
		current: await currentQueueId(page),
		outcome: await outcome(page, 'Coral Night')
	});
	await expect(page.getByTestId('now-playing')).toContainText('Coral Night');
});

test('LIBRESTART-LR19 one polite outcome only: no assertive path and no progress chatter', async ({ page }, info) => {
	await openLibrary(page);
	await mountCoral(page);
	await mockMedia(page, { time: 30, duration: 120 });
	await signalMedia(page);
	await requireHook(page, 'LIBRESTART-LR19');

	await restartButton(page).click();
	await expect.poll(() => timeOf(page)).toBe(0);
	const afterRestart = await statusText(page);
	const livePaths = await page
		.locator('[role="status"], [role="alert"], [aria-live="polite"], [aria-live="assertive"]')
		.evaluateAll((nodes, text) =>
			nodes.filter(
				(node) =>
					node.getAttribute('aria-live') !== 'off' &&
					node.getClientRects().length > 0 &&
					text.length > 0 &&
					(node.textContent ?? '').replace(/\s+/g, ' ').trim() === text
			).length,
			afterRestart
		);
	const assertivePaths = await page
		.locator('[role="alert"], [aria-live="assertive"]')
		.evaluateAll(
			(nodes, text) => nodes.filter((node) => text.length > 0 && (node.textContent ?? '').includes(text)).length,
			afterRestart
		);

	for (const seconds of [1, 2, 3, 10]) await setTime(page, seconds);

	await check(info, {
		statusCount: 1,
		livePaths: 1,
		assertivePaths: 0,
		unchangedAfterProgress: true
	}, {
		statusCount: await restartStatus(page).count(),
		livePaths,
		assertivePaths,
		unchangedAfterProgress: (await statusText(page)) === afterRestart
	});
});
