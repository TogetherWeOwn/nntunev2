import { expect, test, type Page } from '@playwright/test';

/**
 * Acceptance-owned, test-only handoff: one-shot Shuffle upcoming.
 * Builder owns implementation, GREEN and the combined ready PR. These new paths
 * and all existing tests are protected; return an incorrect assertion to its author.
 *
 * From repo root (offline locked dependencies):
 * pnpm --dir apps/web install --offline --frozen-lockfile --prod=false
 * pnpm --dir apps/web test src/lib/player/queue.shuffle-upcoming.test.ts --reporter=json
 * pnpm --dir apps/web test:e2e tests/shuffle-upcoming.spec.ts --workers=1 --reporter=json
 * pnpm --dir apps/web check
 * Baselines: pnpm --dir apps/web test
 * pnpm --dir apps/web test:e2e tests/player-queue.spec.ts tests/player-lifecycle.spec.ts tests/radio-continue.spec.ts tests/player-comforts.spec.ts --workers=1
 *
 * Playwright builds/owns the local preview through its existing webServer config.
 * Port 4173 must belong to THIS checkout (use CI=1 to refuse silent server reuse).
 * If Chromium needs host libraries, use the installed chrome-deps LD_LIBRARY_PATH;
 * do not install packages or change production configuration for this test.
 *
 * Contract hooks: accessible button name "Shuffle upcoming"; an announced
 * role=status completion mentioning shuffle and upcoming/queue. No wording or
 * implementation-specific button test ID is pinned. Existing queue/player hooks
 * remain unchanged. Namespace-loaded pure companion pins the injectable RNG seam.
 *
 * Media/network are synthetic (held responses and native-boundary spies), not
 * evidence of real decoding, live-library streaming, staging or production.
 * The existing demo has three tracks: enough for two upcoming at current=first;
 * the pure companion uses FIVE synthetic tracks for a nonempty played prefix
 * plus multiple upcoming entries. No catalogue expansion/backend fixture needed.
 *
 * Baseline RED must be the visible-control assertion AFTER queue/audio fixture
 * preconditions, not navigation/build/import failure. Subsequent invariants become
 * reachable when Builder adds that capability. No standalone red-CI PR.
 * No new CLI/API, so dry-run is N/A. JSON reporters carry stable UP-SHUF case IDs.
 */

type Probe = {
	playCalls: number;
	pauseCalls: number;
	loadCalls: number;
	seekCalls: number;
	pending: boolean;
	resolve: () => void;
	audio: HTMLAudioElement | null;
};

declare global {
	interface Window {
		__shuffleProbe: Probe;
	}
}

const INITIAL = ['demo-track', 'second-signal', 'third-light'];
const QUEUE_KEY = 'nntune:demo-queue:v1';
const shuffleButton = (page: Page) => page.getByRole('button', { name: /^shuffle upcoming$/i });

async function openPlayer(page: Page, pending = false): Promise<void> {
	await page.route('**/api/demo/audio**', () => {});
	await page.addInitScript((holdPlay) => {
		const probe: Probe = {
			playCalls: 0, pauseCalls: 0, loadCalls: 0, seekCalls: 0,
			pending: holdPlay, resolve: () => {}, audio: null
		};
		window.__shuffleProbe = probe;
		// Only the external media boundary is doubled; real queue, persistence,
		// playback state machine, DOM and event handlers run unchanged.
		const times = new WeakMap<HTMLMediaElement, number>();
		Object.defineProperty(HTMLMediaElement.prototype, 'currentTime', {
			configurable: true,
			get() { return times.get(this) ?? 0; },
			set(value: number) { probe.seekCalls += 1; times.set(this, value); }
		});
		HTMLMediaElement.prototype.play = function () {
			probe.playCalls += 1;
			Object.defineProperty(this, 'paused', { value: false, configurable: true });
			Object.defineProperty(this, 'ended', { value: false, configurable: true });
			if (!probe.pending) return Promise.resolve();
			return new Promise<void>((resolve) => { probe.resolve = resolve; });
		};
		HTMLMediaElement.prototype.pause = function () {
			probe.pauseCalls += 1;
			Object.defineProperty(this, 'paused', { value: true, configurable: true });
		};
		HTMLMediaElement.prototype.load = function () { probe.loadCalls += 1; };
		// Deterministic injection is transient, never persisted as a mode/seed.
		Math.random = () => 0;
	}, pending);
	await page.goto('/demo/player?radio-seed=7');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	await expect.poll(() => queueOrder(page)).toEqual(INITIAL);
}

async function queueOrder(page: Page): Promise<(string | null)[]> {
	return page.getByTestId('queue-item').evaluateAll(
		(items) => items.map((item) => item.getAttribute('data-track-id'))
	);
}

async function storage(page: Page) {
	return page.evaluate(() => ({
		local: Object.fromEntries(Object.entries(localStorage)),
		session: Object.fromEntries(Object.entries(sessionStorage))
	}));
}

async function prepareAudio(page: Page, intent: 'playing' | 'paused' | 'loading', staleResume = true) {
	await page.getByTestId('queue-select-demo-track').click();
	if (intent === 'loading') {
		await expect(page.getByTestId('player-status')).toHaveText('Status: loading');
	} else {
		await page.getByTestId('demo-audio').evaluate((el) => el.dispatchEvent(new Event('playing')));
		await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	}
	await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement, pause) => {
		if (pause) {
			el.pause();
			el.dispatchEvent(new Event('pause'));
		}
		el.currentTime = 42.5;
		el.dispatchEvent(new Event('timeupdate'));
		window.__shuffleProbe.audio = el;
	}, intent === 'paused');
	if (intent === 'paused') await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
	// A stale resume value must not be applied by a queue-only edit.
	if (staleResume) {
		await page.evaluate(() => localStorage.setItem('nntune:resume:v1', '{"demo-track":20}'));
	}
}

async function audioSnapshot(page: Page) {
	return page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => {
		const probe = window.__shuffleProbe;
		return {
			sameAudio: el === probe.audio,
			src: el.getAttribute('src'), time: el.currentTime, paused: el.paused,
			playCalls: probe.playCalls, pauseCalls: probe.pauseCalls,
			loadCalls: probe.loadCalls, seekCalls: probe.seekCalls
		};
	});
}

async function requireShuffle(page: Page) {
	const button = shuffleButton(page);
	await expect(button, 'UP-SHUF: existing player exposes the one-shot Shuffle upcoming button').toHaveCount(1);
	await expect(button).toBeVisible();
	return button;
}

for (const intent of ['playing', 'paused', 'loading'] as const) {
	test(`UP-SHUF-B01 ${intent}: repeated shuffles preserve current audio, time and play intent without requests/radio`, async ({ page }) => {
		await openPlayer(page, intent === 'loading');
		await prepareAudio(page, intent);
		const before = await audioSnapshot(page);
		expect(before.sameAudio).toBe(true);
		expect(before.time).toBe(42.5);
		expect(before.playCalls).toBeGreaterThan(0);
		const status = await page.getByTestId('player-status').textContent();
		const button = await requireShuffle(page);
		const requests: string[] = [];
		page.on('request', (request) => requests.push(request.url()));
		for (let click = 0; click < 3; click += 1) {
			await expect(button).toBeEnabled();
			await button.click();
			const order = await queueOrder(page);
			expect(order[0]).toBe('demo-track');
			expect(order.slice(1).sort()).toEqual(['second-signal', 'third-light']);
			await expect(page.getByTestId('now-playing')).toContainText('Demo track');
			await expect(page.getByTestId('player-status')).toHaveText(status!);
			await expect(page.getByTestId('radio-badge')).toHaveCount(0);
			expect(await audioSnapshot(page)).toEqual(before);
		}
		expect(requests).toEqual([]);
		if (intent === 'loading') {
			await page.evaluate(() => window.__shuffleProbe.resolve());
			await page.getByTestId('demo-audio').evaluate((el) => el.dispatchEvent(new Event('playing')));
			await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
			expect(await audioSnapshot(page)).toEqual(before);
		}
	});
}

for (const state of ['empty', 'one-track', 'two-tracks', 'middle', 'last'] as const) {
	test(`UP-SHUF-B02 ${state}: disable when fewer than two upcoming remain`, async ({ page }) => {
		await openPlayer(page);
		if (state === 'empty' || state === 'one-track' || state === 'two-tracks') {
			await page.getByTestId('queue-clear').click();
			if (state !== 'empty') await page.getByTestId('queue-add-demo-track').click();
			if (state === 'two-tracks') await page.getByTestId('queue-add-second-signal').click();
			await expect(page.getByTestId('queue-item')).toHaveCount(
				state === 'empty' ? 0 : state === 'one-track' ? 1 : 2
			);
		} else {
			await page.getByTestId(`queue-select-${state === 'middle' ? 'second-signal' : 'third-light'}`).click();
			await expect(page.getByTestId('now-playing')).toContainText(state === 'middle' ? 'Second Signal' : 'Third Light');
		}
		const button = await requireShuffle(page);
		await expect(button).toBeDisabled();
	});
}

test('UP-SHUF-B03: active radio disables explicit-queue shuffle without editing its order', async ({ page }) => {
	await openPlayer(page);
	await page.getByTestId('queue-select-third-light').click();
	await page.getByTestId('demo-audio').evaluate((el) => {
		Object.defineProperty(el, 'paused', { value: true, configurable: true });
		Object.defineProperty(el, 'ended', { value: true, configurable: true });
		el.dispatchEvent(new Event('pause'));
		el.dispatchEvent(new Event('ended'));
	});
	await expect(page.getByTestId('radio-badge')).toBeVisible();
	expect(await queueOrder(page)).toEqual(INITIAL);
	const button = await requireShuffle(page);
	await expect(button).toBeDisabled();
	await page.getByTestId('radio-stop').click();
	await expect(page.getByTestId('radio-badge')).toHaveCount(0);
	expect(await queueOrder(page)).toEqual(INITIAL);
});

test('UP-SHUF-B04: keyboard activation has visible focus, announces completion and does not steal focus', async ({ page }) => {
	await openPlayer(page);
	const button = await requireShuffle(page);
	await expect(button).toBeEnabled();
	for (let tab = 0; tab < 40; tab += 1) {
		await page.keyboard.press('Tab');
		if (await button.evaluate((el) => el === document.activeElement)) break;
	}
	await expect(button).toBeFocused();
	expect(await button.evaluate((el) => {
		const css = getComputedStyle(el);
		return el.matches(':focus-visible') && (
			(css.outlineStyle !== 'none' && Number.parseFloat(css.outlineWidth) > 0) || css.boxShadow !== 'none'
		);
	})).toBe(true);
	// Both native button activation keys must work, without any focus transfer.
	for (const key of ['Enter', 'Space']) {
		await page.keyboard.press(key);
		const completion = page.getByRole('status').filter({ hasText: /shuffl/i });
		await expect(completion).toHaveCount(1);
		await expect(completion).toContainText(/upcoming|queue/i);
		await expect(completion).not.toContainText(/guaranteed|different order|new order|order changed/i);
		await expect(button).toBeFocused();
		expect((await queueOrder(page)).slice(1).sort()).toEqual(['second-signal', 'third-light']);
	}
});

test('UP-SHUF-B05: existing session schema persists the resulting order/current ID and reload restores without autoplay', async ({ page }) => {
	await openPlayer(page);
	await prepareAudio(page, 'paused', false);
	const button = await requireShuffle(page);
	const before = await storage(page);
	await button.click();
	const order = await queueOrder(page);
	expect(order[0]).toBe('demo-track');
	expect(order.slice(1).sort()).toEqual(['second-signal', 'third-light']);
	await expect.poll(() => page.evaluate((key) => JSON.parse(sessionStorage.getItem(key) ?? 'null'), QUEUE_KEY))
		.toEqual({ orderIds: order, currentId: 'demo-track' });
	const after = await storage(page);
	expect(after.local).toEqual(before.local);
	expect(Object.keys(after.session).sort()).toEqual(Object.keys(before.session).sort());
	for (const key of Object.keys(before.session).filter((key) => key !== QUEUE_KEY)) {
		expect(after.session[key]).toBe(before.session[key]);
	}
	await page.reload();
	await expect.poll(() => queueOrder(page)).toEqual(order);
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	await expect.poll(() => page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => el.currentTime)).toBe(42.5);
	expect(await page.evaluate(() => window.__shuffleProbe.playCalls)).toBe(0);
	await expect(page.getByTestId('radio-badge')).toHaveCount(0);
	await expect(shuffleButton(page)).toBeEnabled();
});

test('UP-SHUF-C01 control: synthetic media retains real queue/session/resume behavior across reload', async ({ page }) => {
	await openPlayer(page);
	await prepareAudio(page, 'paused', false);
	expect(await audioSnapshot(page)).toMatchObject({ sameAudio: true, time: 42.5, paused: true });
	await expect.poll(() => page.evaluate((key) => JSON.parse(sessionStorage.getItem(key) ?? 'null'), QUEUE_KEY))
		.toEqual({ orderIds: INITIAL, currentId: 'demo-track' });
	await page.reload();
	await expect.poll(() => queueOrder(page)).toEqual(INITIAL);
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	await expect.poll(() => page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => el.currentTime)).toBe(42.5);
	expect(await page.evaluate(() => window.__shuffleProbe.playCalls)).toBe(0);
	await expect(page.getByTestId('radio-badge')).toHaveCount(0);
});

test('UP-SHUF-B06: manual moves still work after shuffle and transport remains clamped', async ({ page }) => {
	await openPlayer(page);
	const button = await requireShuffle(page);
	await button.click();
	const order = await queueOrder(page);
	const last = order[2]!;
	await page.getByTestId(`queue-move-up-${last}`).click();
	expect(await queueOrder(page)).toEqual([order[0], order[2], order[1]]);
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	await page.getByTestId('player-prev').click();
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	await page.getByTestId('player-next').click();
	await page.getByTestId('player-next').click();
	// Terminal Next changes the status label, not the selected queue entry.
	const atEnd = page.getByTestId(`queue-select-${order[1]}`);
	await expect(atEnd).toHaveAttribute('aria-current', 'true');
	await page.getByTestId('player-next').click();
	await expect(atEnd).toHaveAttribute('aria-current', 'true');
	await expect(page.getByTestId('player-status')).toHaveText('Status: ended');
	await expect(page.getByTestId('radio-badge')).toHaveCount(0);
	await page.getByTestId('queue-clear').click();
	await page.getByTestId('queue-add-third-light').click();
	await page.getByTestId('queue-add-demo-track').click();
	expect(await queueOrder(page)).toEqual(['third-light', 'demo-track']);
	await expect(page.getByTestId('now-playing')).toContainText('Third Light');
	await expect(button).toBeDisabled();
});
