import { expect, test, type Page, type TestInfo } from '@playwright/test';

/**
 * Acceptance-owned RED bank: demo queue Play next. Do not edit in the build half.
 *
 * Builder-owned implementation / protected from this test-only change:
 *   apps/web/src/routes/demo/player/+page.svelte
 *   apps/web/src/lib/player/queue.ts (reuse playNext; no new queue algorithm)
 * Existing queue suites and library files remain unchanged.
 *
 * GREEN contract:
 * - Every demo row has a native button, queue-play-next-{track.id}, with
 *   accessible name "Play {title} next". Like library rows, current/already-next
 *   buttons remain enabled; invoking them is a silent no-op.
 * - Invoke the existing pure playNext(queue, index). Literal tables below pin
 *   the observable semantics independently of that helper: preserve current
 *   identity, move only the requested track, preserve every other relative order.
 * - queue-announcement is a polite live region in the demo queue panel (role
 *   status supplies implicit polite). Moves name the track and "position N of M";
 *   no exact sentence is required. No-ops leave text AND live-region mutations
 *   untouched, including when a previous move's announcement is still present.
 * - Keyed row focus stays on the SAME invoking control, even when it moves into
 *   the already-next slot. No selecting, restarting, seeking or recording plays.
 * - Enter/Space and narrow touch work without horizontal page overflow.
 *
 * Only media loading/play/pause are doubled; real queue, DOM, persistence and
 * event delivery run. These are synthetic local demo flows, never production.
 * Missing controls yield ordinary expected/actual assertion RED, not click
 * timeouts. Each case emits one compact JSON receipt and attaches it to its
 * Playwright result (expected/actual/status/error_code/next_action).
 *
 * Focused, offline after cached dependencies are installed:
 *   pnpm --dir apps/web exec playwright test tests/demo-play-next.spec.ts --workers=1
 * Existing pure-helper regression control (expected GREEN before and after):
 *   pnpm --dir apps/web exec vitest run src/lib/player/queue.test.ts
 * The standard Playwright config builds/serves its own local app. When port 4173
 * is occupied by another worktree, use a run-owned config with a free port and
 * reuseExistingServer:false; never test a different checkout's preview.
 *
 * Breaks caught: absent/wrong handler; move-to-top instead of after-current;
 * stale row indexes; stolen selection/restarted media; duplicate tracks;
 * chatter on no-op; missing/wrong position announcement; lost keyed focus;
 * mouse-only control; narrow-screen overflow. No source-text pins.
 */

const IDS = ['demo-track', 'second-signal', 'third-light'];
const TITLES: Record<string, string> = {
	'demo-track': 'Demo track',
	'second-signal': 'Second Signal',
	'third-light': 'Third Light'
};
const QUEUE_KEY = 'nntune:demo-queue:v1';

type Gesture = 'click' | 'Enter' | 'Space' | 'tap';

async function openPlayer(page: Page, order = IDS, currentId = 'demo-track'): Promise<void> {
	await page.route('**/api/demo/audio**', () => {});
	await page.addInitScript(({ key, orderIds, currentId }) => {
		sessionStorage.setItem(key, JSON.stringify({ orderIds, currentId }));
		HTMLMediaElement.prototype.play = function () { return Promise.resolve(); };
		HTMLMediaElement.prototype.pause = function () {};
	}, { key: QUEUE_KEY, orderIds: order, currentId });
	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(order.length);
	await expect(page.getByTestId(`queue-select-${currentId}`)).toHaveAttribute('aria-current', 'true');
	// A client effect publishes MediaSession metadata only after hydration.
	await expect.poll(() => page.evaluate(() => navigator.mediaSession.metadata?.title)).toBe(TITLES[currentId]);
	await expect(page.getByTestId('recently-played-empty')).toBeVisible();
}

async function state(page: Page) {
	return page.evaluate(() => {
		const audio = document.querySelector<HTMLAudioElement>('[data-testid="demo-audio"]');
		return {
			order: [...document.querySelectorAll('[data-testid="queue-item"]')].map(el => el.getAttribute('data-track-id')),
			selected: [...document.querySelectorAll('[data-testid="queue-item"]')]
				.filter(el => el.querySelector('[aria-current="true"]') || el.getAttribute('aria-current') === 'true')
				.map(el => el.getAttribute('data-track-id')),
			nowPlaying: document.querySelector('[data-testid="now-playing"]')?.textContent?.trim(),
			status: document.querySelector('[data-testid="player-status"]')?.textContent?.trim(),
			src: audio?.getAttribute('src'),
			time: audio?.currentTime,
			history: sessionStorage.getItem('nntune:demo-recently-played:v1'),
			resume: localStorage.getItem('nntune:resume:v1')
		};
	});
}

async function invoke(page: Page, id: string, gesture: Gesture = 'click'): Promise<boolean> {
	const control = page.getByTestId(`queue-play-next-${id}`);
	if (await control.count() !== 1) return false;
	await expect(control).toHaveRole('button');
	await expect(control).toHaveJSProperty('tagName', 'BUTTON');
	await expect(control).toHaveAccessibleName(`Play ${TITLES[id]} next`);
	await expect(control).toBeEnabled();
	if (gesture === 'Enter' || gesture === 'Space') {
		await control.focus();
		await control.press(gesture);
	} else if (gesture === 'tap') {
		await control.tap();
	} else {
		await control.click();
	}
	return true;
}

async function announcement(page: Page) {
	const region = page.getByTestId('queue-panel').getByTestId('queue-announcement');
	if (await region.count() !== 1) return null;
	const text = (await region.textContent() ?? '').trim();
	const position = /position\s+(\d+)\s+of\s+(\d+)/i.exec(text);
	const live = await region.getAttribute('aria-live');
	return {
		track: IDS.find(id => text.includes(TITLES[id])) ?? null,
		position: position ? Number(position[1]) : null,
		total: position ? Number(position[2]) : null,
		polite: live === 'polite' || (live === null && await region.getAttribute('role') === 'status')
	};
}

async function unchangedPlayback(page: Page, before: Awaited<ReturnType<typeof state>>) {
	const after = await state(page);
	const { order: _beforeOrder, selected: _beforeSelected, ...oldPlayback } = before;
	const { order: _afterOrder, selected: _afterSelected, ...newPlayback } = after;
	return JSON.stringify(newPlayback) === JSON.stringify(oldPlayback);
}

async function receipt(page: Page, info: TestInfo, expected: unknown, action: () => Promise<unknown>) {
	const caseId = info.title.split(' ')[0];
	let actual: unknown = null;
	let failure: unknown;
	try {
		actual = await action();
		expect(actual).toEqual(expected);
	} catch (error) {
		failure = error;
		if (actual === null) actual = { observed: await state(page), error: String(error).slice(0, 800) };
		throw error;
	} finally {
		const result = {
			case_id: caseId, expected, actual,
			status: failure ? 'RED' : 'GREEN',
			error_code: failure ? 'DEMO_PLAY_NEXT_CONTRACT' : null,
			next_action: failure ? 'Builder: wire demo rows to playNext and preserve the pinned behavior; do not edit this test bank.' : 'none'
		};
		const json = JSON.stringify(result);
		console.log(json);
		await info.attach(`${caseId}.json`, { body: json, contentType: 'application/json' });
	}
}

for (const scenario of [
	{ id: 'DPN-01', name: 'last upcoming track moves immediately after current', current: 'demo-track', target: 'third-light', order: ['demo-track', 'third-light', 'second-signal'], position: 2 },
	{ id: 'DPN-02', name: 'earlier first track moves after nonzero current, not to top', current: 'second-signal', target: 'demo-track', order: ['second-signal', 'demo-track', 'third-light'], position: 2 },
	{ id: 'DPN-03', name: 'earlier middle track moves after the last current', current: 'third-light', target: 'second-signal', order: ['demo-track', 'third-light', 'second-signal'], position: 3 }
]) {
	test(`${scenario.id} ${scenario.name}`, async ({ page }, info) => {
		await openPlayer(page, IDS, scenario.current);
		const before = await state(page);
		await receipt(page, info, {
			invoked: true, order: scenario.order, selected: [scenario.current], playbackUnchanged: true,
			announcement: { track: scenario.target, position: scenario.position, total: 3, polite: true }, focusKept: true
		}, async () => {
			const invoked = await invoke(page, scenario.target);
			const after = await state(page);
			return {
				invoked, order: after.order, selected: after.selected,
				playbackUnchanged: await unchangedPlayback(page, before), announcement: await announcement(page),
				focusKept: await page.getByTestId(`queue-play-next-${scenario.target}`).evaluateAll(els => els.length === 1 && els[0] === document.activeElement)
			};
		});
	});
}

for (const scenario of [
	{ id: 'DPN-04', name: 'current row is a silent no-op', order: IDS, target: 'demo-track' },
	{ id: 'DPN-05', name: 'already-next row is a silent no-op', order: IDS, target: 'second-signal' },
	{ id: 'DPN-06', name: 'single-track current is a silent no-op', order: ['demo-track'], target: 'demo-track' }
]) {
	test(`${scenario.id} ${scenario.name}`, async ({ page }, info) => {
		await openPlayer(page, scenario.order);
		const before = await state(page);
		await observeLiveMutations(page);
		await receipt(page, info, { invoked: true, stateUnchanged: true, announcementPresent: true, announcementEmpty: true, liveMutations: [] }, async () => {
			const invoked = await invoke(page, scenario.target);
			await settle(page);
			return {
				invoked, stateUnchanged: JSON.stringify(await state(page)) === JSON.stringify(before),
				announcementPresent: await page.getByTestId('queue-announcement').count() === 1,
				announcementEmpty: await page.getByTestId('queue-announcement').evaluateAll(els => els.length === 1 && !els[0]?.textContent?.trim()),
				liveMutations: await liveMutations(page)
			};
		});
	});
}

// Observe actual DOM writes, not just final text: clearing then restoring the
// same announcement is chatter too. All existing demo live regions are watched.
async function observeLiveMutations(page: Page): Promise<void> {
	await page.evaluate(() => {
		const changes: string[] = [];
		const observer = new MutationObserver(records => {
			for (const record of records) changes.push(record.target.textContent ?? '');
		});
		for (const el of document.querySelectorAll('[role="status"], [aria-live]')) {
			observer.observe(el, { childList: true, characterData: true, subtree: true });
		}
		Object.assign(window, { __playNextChanges: changes });
	});
}

async function liveMutations(page: Page): Promise<string[]> {
	return page.evaluate(() => (window as unknown as { __playNextChanges: string[] }).__playNextChanges);
}

async function settle(page: Page): Promise<void> {
	// Bounded negative-observation window, not an external-service polling loop.
	await page.waitForTimeout(200);
}

test('DPN-07 already-next and current preserve a previous move announcement without chatter', async ({ page }, info) => {
	await openPlayer(page);
	await receipt(page, info, { firstInvoked: true, noopsInvoked: [true, true], stateUnchanged: true, announcementKept: true, liveMutations: [] }, async () => {
		const firstInvoked = await invoke(page, 'third-light');
		await settle(page);
		const before = await state(page);
		const prior = await announcement(page);
		await observeLiveMutations(page);
		const noopsInvoked = [await invoke(page, 'third-light'), await invoke(page, 'demo-track')];
		await settle(page);
		return {
			firstInvoked, noopsInvoked,
			stateUnchanged: JSON.stringify(await state(page)) === JSON.stringify(before),
			announcementKept: prior?.track === 'third-light' && JSON.stringify(await announcement(page)) === JSON.stringify(prior),
			liveMutations: await liveMutations(page)
		};
	});
});

for (const key of ['Enter', 'Space'] as const) {
	test(`DPN-${key === 'Enter' ? '08' : '09'} ${key} moves the row and keeps the invoking control focused`, async ({ page }, info) => {
		await openPlayer(page);
		await receipt(page, info, { invoked: true, order: ['demo-track', 'third-light', 'second-signal'], focusKept: true }, async () => {
			const invoked = await invoke(page, 'third-light', key);
			return {
				invoked, order: (await state(page)).order,
				focusKept: await page.getByTestId('queue-play-next-third-light').evaluateAll(els => els.length === 1 && els[0] === document.activeElement)
			};
		});
	});
}

test('DPN-10 subsequent requests use the new row index; Next actually plays the requested track', async ({ page }, info) => {
	await openPlayer(page);
	await receipt(page, info, {
		invoked: [true, true], order: ['demo-track', 'second-signal', 'third-light'],
		announcement: { track: 'second-signal', position: 2, total: 3, polite: true },
		playedRequested: true, selected: ['second-signal']
	}, async () => {
		const invoked = [await invoke(page, 'third-light'), await invoke(page, 'second-signal')];
		const movedAnnouncement = await announcement(page);
		await page.getByTestId('player-next').click();
		await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
		const after = await state(page);
		return { invoked, order: after.order, announcement: movedAnnouncement, playedRequested: after.src === '/api/demo/audio?track=second-signal', selected: after.selected };
	});
});

test('DPN-12 a move preserves live playback time, history and the media element', async ({ page }, info) => {
	await openPlayer(page);
	await page.getByTestId('queue-select-demo-track').click();
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => {
		el.currentTime = 37.25;
		el.dispatchEvent(new Event('timeupdate'));
		Object.assign(window, { __playNextAudio: el });
	});
	const before = await state(page);
	expect(before.time).toBe(37.25);
	await receipt(page, info, { invoked: true, order: ['demo-track', 'third-light', 'second-signal'], selected: ['demo-track'], playbackUnchanged: true, sameAudio: true }, async () => {
		const invoked = await invoke(page, 'third-light');
		const after = await state(page);
		return {
			invoked, order: after.order, selected: after.selected,
			playbackUnchanged: await unchangedPlayback(page, before),
			sameAudio: await page.getByTestId('demo-audio').evaluate(el => el === (window as unknown as { __playNextAudio: HTMLAudioElement }).__playNextAudio)
		};
	});
});

test.describe('narrow touch', () => {
	test.use({ viewport: { width: 320, height: 740 }, hasTouch: true, isMobile: true });
	test('DPN-11 touch control stays usable without horizontal document overflow', async ({ page }, info) => {
		await openPlayer(page);
		await receipt(page, info, { invoked: true, order: ['demo-track', 'third-light', 'second-signal'], withinViewport: true, noHorizontalOverflow: true }, async () => {
			const invoked = await invoke(page, 'third-light', 'tap');
			return {
				invoked, order: (await state(page)).order,
				withinViewport: await page.getByTestId('queue-play-next-third-light').evaluateAll(els => {
					if (els.length !== 1) return false;
					const box = els[0]!.getBoundingClientRect();
					return box.width > 0 && box.height > 0 && box.left >= 0 && box.right <= window.innerWidth;
				}),
				noHorizontalOverflow: await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)
			};
		});
	});
});
