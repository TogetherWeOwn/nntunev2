import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Acceptance: remove one demo queue track without clearing the queue.
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build lands the
 * demo per-row Remove on top of this branch and must make these tests
 * pass WITHOUT editing this file; a wrong test comes back to the
 * acceptance author instead.
 *
 * RED base: 781c4b657d3621e01c0a2f7d6e8cdc806db8ea04 (2026-10-24).
 * Observed: DM-B-01..DM-B-10 assertion-RED (no Remove buttons, no
 * announcement region, no empty-focus target on the demo route);
 * DM-B-00/DM-B-11 controls pass; no harness errors.
 *
 * Harness (same mocked-demo pattern as the neighbouring player specs):
 * `/demo/player` opens with cleared storage (the 3-track fixture queue),
 * demo media held pending (fulfilling garbage bytes would fire native
 * error events), play/pause stubbed with a play-call counter, and a
 * same-origin traffic guard. Every test guards its first control
 * interaction with an explicit visibility/attachment assertion, so a run
 * against a build without the controls fails as an assertion (missing
 * control), never as a click error.
 *
 * Hooks (route literals — the e2e TESTIDS map does not cover this
 * route — following the queue-select-/queue-move-up- convention):
 *   queue-remove-{track.id} ... native <button>, exact accessible name
 *                                "Remove {title} from queue"
 *   queue-announcement ......... shared polite region, role="status",
 *                                renders its text verbatim (shared with
 *                                the queue move-to-top/bottom slice)
 *   queue-empty-focus .......... stable non-activatable target with
 *                                tabindex="-1" for sole-removal focus
 *                                (same contract as the queue remove/clear
 *                                focus slice for PlayerQueue)
 *
 * Contract under test (stable case IDs DM-B-00..DM-B-11):
 * - Every demo queue-item row offers Remove; removing a non-current row
 *   keeps order and the current track, and never clears the queue.
 * - Removing the current track advances to the next track (tail falls
 *   back to the new last track; sole removal empties) with zero new
 *   play() calls — advancing selection never restarts playback.
 * - Each removal announces "{title} removed from the queue" exactly once
 *   through queue-announcement, replacing any earlier message.
 * - Focus recovers per the queue remove/clear focus semantics: the next
 *   surviving row's Remove, else the previous, else queue-empty-focus —
 *   never the body, never the now-disabled Clear. Recovery fires only
 *   when the removed control held focus; consecutive Enter removals stay
 *   operable until the queue empties.
 * - Remove is keyboard, touch and narrow-screen (360px) operable;
 *   selection, moves, transport, Clear and re-add keep working.
 *
 * Out of scope (not pinned): library rows, bulk trim, undo, radio/
 * repeat/sleep accounting, demo Clear announcements, visual theming.
 *
 * From apps/web (Node 24 / pnpm 10):
 *   pnpm install --frozen-lockfile
 *   CI=1 pnpm test:e2e tests/demo-queue-remove.spec.ts --workers=1
 * Regression: same command with tests/player-restart.spec.ts and
 *   tests/library-player-queue.spec.ts; plus the unit companion
 *   (pnpm test src/routes/demo/player/demoQueueRemove.acceptance.test.ts),
 *   pnpm test src/lib/player/queue.test.ts and pnpm check.
 * Ensure preview port 4173 is free; CI=1 forces this run's own server,
 * never another worktree's preview.
 * Expected pre-feature RED: DM-B-01..DM-B-10 fail on assertions naming
 * the missing Remove/announcement/empty-target hooks and their next
 * action, while DM-B-00/DM-B-11 stay green. Builder next action:
 * implement the demo removeAt handler reusing removeTrack plus the
 * announcement and focus recovery, then run this file plus the
 * protected regressions and same-head PR gates.
 */

test.use({ viewport: { width: 1000, height: 700 }, hasTouch: true });

const DEMO_TITLE: Record<string, string> = {
	'demo-track': 'Demo track',
	'second-signal': 'Second Signal',
	'third-light': 'Third Light'
};

const DEMO_STREAM: Record<string, string> = {
	'demo-track': '/api/demo/audio',
	'second-signal': '/api/demo/audio?track=second-signal',
	'third-light': '/api/demo/audio?track=third-light'
};

interface Traffic {
	apiCalls: string[];
	foreign: string[];
}

/** Same-origin only; demo media is held pending, never fulfilled. */
async function localOnly(page: Page, origin: string, traffic: Traffic): Promise<void> {
	const url = new URL(origin);
	expect(['localhost', '127.0.0.1', '[::1]']).toContain(url.hostname);
	await page.route('**/*', async (route) => {
		const request = route.request();
		const target = new URL(request.url());
		if (target.origin !== url.origin) {
			traffic.foreign.push(request.url());
			return route.abort();
		}
		if (target.pathname.startsWith('/api/')) {
			traffic.apiCalls.push(`${request.method()} ${target.pathname}`);
			if (target.pathname.startsWith('/api/demo/audio')) return;
			return route.continue();
		}
		return route.continue();
	});
}

function expectDemoTraffic(traffic: Traffic): void {
	expect(traffic.foreign).toEqual([]);
	expect(
		traffic.apiCalls.every(
			(call) => call.startsWith('GET /api/demo/audio') || call.startsWith('GET /api/demo/')
		)
	).toBe(true);
}

async function openDemo(page: Page, baseURL: string, traffic: Traffic): Promise<void> {
	await page.addInitScript(() => {
		sessionStorage.clear();
		localStorage.clear();
		const lifecycle = { playCalls: 0 };
		Object.assign(window, { __demoRemoveLifecycle: lifecycle });
		HTMLMediaElement.prototype.play = function () {
			lifecycle.playCalls += 1;
			return Promise.resolve();
		};
		HTMLMediaElement.prototype.pause = function () {};
	});
	await localOnly(page, baseURL, traffic);
	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
}

async function playCallsOf(page: Page): Promise<number> {
	return page.evaluate(
		() =>
			(window as unknown as { __demoRemoveLifecycle: { playCalls: number } })
				.__demoRemoveLifecycle.playCalls
	);
}

function queueIds(page: Page): Promise<(string | null)[]> {
	return page
		.getByTestId('queue-item')
		.evaluateAll((rows) => rows.map((row) => row.getAttribute('data-track-id')));
}

/** The demo current marker lives on the per-row select control. */
function currentSelect(page: Page): Locator {
	return page.locator('[data-testid^="queue-select-"][aria-current="true"]');
}

function removeButton(page: Page, trackId: string): Locator {
	return page.getByTestId(`queue-remove-${trackId}`);
}

function announcement(page: Page): Locator {
	return page.getByTestId('queue-announcement');
}

function emptyTarget(page: Page): Locator {
	return page.getByTestId('queue-empty-focus');
}

async function activeIdentity(page: Page): Promise<{ tag: string; label: string; track: string }> {
	return page.evaluate(() => {
		const el = document.activeElement as HTMLElement | null;
		const item = el?.closest?.('[data-testid="queue-item"]');
		return {
			tag: el?.tagName ?? '',
			label: (el?.getAttribute('aria-label') ?? el?.textContent ?? '').trim().slice(0, 80),
			track: item?.getAttribute('data-track-id') ?? ''
		};
	});
}

/** The recovered control is a usable native button, not a new tab stop. */
async function expectUsableRemove(target: Locator): Promise<void> {
	await expect(target).toBeFocused();
	await expect(target).toBeVisible();
	await expect(target).toHaveAttribute('type', 'button');
	expect(await target.getAttribute('tabindex')).toBeNull();
	await expect(target).toBeEnabled();
}

/** The empty-queue target is focused, visible and programmatically focusable. */
async function expectEmptyRecovery(page: Page): Promise<void> {
	const target = emptyTarget(page);
	await expect(target).toBeFocused();
	await expect(target).toBeVisible();
	await expect(target).toHaveAttribute('tabindex', '-1');
	const tag = await target.evaluate((el) => el.tagName);
	expect(tag).not.toMatch(/^(BUTTON|A|INPUT|SELECT|TEXTAREA)$/);
	expect(((await target.textContent()) ?? '').trim()).toMatch(/queue|empty|clear/i);
	const identity = await activeIdentity(page);
	expect(identity.tag).not.toBe('BODY');
	expect(identity.track).toBe('');
}

/** Record one entry per live-region commit so "announced once" is countable. */
async function watchAnnouncements(page: Page): Promise<void> {
	await page.evaluate(() => {
		const region = document.querySelector('[data-testid="queue-announcement"]');
		(window as unknown as { __demoRemoveCommits: string[] }).__demoRemoveCommits = [];
		if (!region) return;
		new MutationObserver(() => {
			(window as unknown as { __demoRemoveCommits: string[] }).__demoRemoveCommits.push(
				region.textContent ?? ''
			);
		}).observe(region, { childList: true, characterData: true, subtree: true });
	});
}

async function announcementCommits(page: Page): Promise<string[]> {
	return page.evaluate(
		() => (window as unknown as { __demoRemoveCommits?: string[] }).__demoRemoveCommits ?? []
	);
}

test('DM-B-00 control: the mocked harness seeds three rows with working select/move/clear', async ({
	page,
	baseURL
}) => {
	const traffic: Traffic = { apiCalls: [], foreign: [] };
	await openDemo(page, baseURL!, traffic);

	expect(await queueIds(page)).toEqual(['demo-track', 'second-signal', 'third-light']);
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	await expect(currentSelect(page)).toHaveAttribute('data-testid', 'queue-select-demo-track');

	await page.getByTestId('queue-select-third-light').click();
	await expect(currentSelect(page)).toHaveAttribute(
		'data-testid',
		'queue-select-third-light'
	);
	await expect(page.getByTestId('now-playing')).toContainText('Third Light');

	await page.getByTestId('queue-move-up-third-light').click();
	expect(await queueIds(page)).toEqual(['demo-track', 'third-light', 'second-signal']);
	await expect(currentSelect(page)).toHaveAttribute(
		'data-testid',
		'queue-select-third-light'
	);

	await page.getByTestId('queue-move-down-third-light').click();
	expect(await queueIds(page)).toEqual(['demo-track', 'second-signal', 'third-light']);

	const clear = page.getByTestId('queue-clear');
	await expect(clear).toBeEnabled();
	await clear.click();
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
	await expect(clear).toBeDisabled();
	expectDemoTraffic(traffic);
});

test('DM-B-01 every row offers Remove; removing the middle row keeps order, current and the queue', async ({
	page,
	baseURL
}) => {
	const traffic: Traffic = { apiCalls: [], foreign: [] };
	await openDemo(page, baseURL!, traffic);

	for (const [id, title] of Object.entries(DEMO_TITLE)) {
		const remove = removeButton(page, id);
		await expect(remove, `row ${id} must offer a per-row Remove (next action: add queue-remove-{id} buttons)`).toBeVisible();
		await expect(remove).toBeEnabled();
		await expect(remove).toHaveAccessibleName(`Remove ${title} from queue`);
	}

	const remove = removeButton(page, 'second-signal');
	await remove.click();

	expect(await queueIds(page)).toEqual(['demo-track', 'third-light']);
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	await expect(currentSelect(page)).toHaveAttribute(
		'data-testid',
		'queue-select-demo-track'
	);
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	// Negative: the queue is trimmed, never cleared — Clear and Next survive.
	await expect(page.getByTestId('queue-clear')).toBeEnabled();
	await expect(page.getByTestId('player-next')).toBeEnabled();
	expect(await playCallsOf(page)).toBe(0);
	expectDemoTraffic(traffic);
});

test('DM-B-02 removing the current first track advances to the next track without restarting playback', async ({
	page,
	baseURL
}) => {
	const traffic: Traffic = { apiCalls: [], foreign: [] };
	await openDemo(page, baseURL!, traffic);
	const playsBefore = await playCallsOf(page);

	const remove = removeButton(page, 'demo-track');
	await expect(remove).toBeVisible();
	await remove.click();

	expect(await queueIds(page)).toEqual(['second-signal', 'third-light']);
	await expect(currentSelect(page)).toHaveAttribute(
		'data-testid',
		'queue-select-second-signal'
	);
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await expect(page.getByTestId('demo-audio')).toHaveAttribute(
		'src',
		DEMO_STREAM['second-signal']
	);
	// Advancing selection issues no play: the status never leaves selected.
	expect(await playCallsOf(page)).toBe(playsBefore);
	await expect(page.getByTestId('player-status')).toContainText('Status: selected');
	// Negative: the removed track is gone, not reselected or duplicated.
	expect(await queueIds(page)).not.toContain('demo-track');
	expectDemoTraffic(traffic);
});

test('DM-B-03 removing the current tail falls back to the new last track without replaying', async ({
	page,
	baseURL
}) => {
	const traffic: Traffic = { apiCalls: [], foreign: [] };
	await openDemo(page, baseURL!, traffic);

	await page.getByTestId('queue-select-third-light').click();
	await expect(currentSelect(page)).toHaveAttribute(
		'data-testid',
		'queue-select-third-light'
	);
	const playsBefore = await playCallsOf(page);

	const remove = removeButton(page, 'third-light');
	await expect(remove).toBeVisible();
	await remove.click();

	expect(await queueIds(page)).toEqual(['demo-track', 'second-signal']);
	await expect(currentSelect(page)).toHaveAttribute(
		'data-testid',
		'queue-select-second-signal'
	);
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	expect(await playCallsOf(page)).toBe(playsBefore);
	expect(await queueIds(page)).not.toContain('third-light');
	expectDemoTraffic(traffic);
});

test('DM-B-04 removing rows down to the sole track empties the queue with a stable empty state', async ({
	page,
	baseURL
}) => {
	const traffic: Traffic = { apiCalls: [], foreign: [] };
	await openDemo(page, baseURL!, traffic);

	const second = removeButton(page, 'second-signal');
	await expect(second).toBeVisible();
	await second.click();
	await expect(page.getByTestId('queue-item')).toHaveCount(2);

	const third = removeButton(page, 'third-light');
	await expect(third).toBeVisible();
	await third.click();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	await expect(currentSelect(page)).toHaveAttribute(
		'data-testid',
		'queue-select-demo-track'
	);

	const sole = removeButton(page, 'demo-track');
	await expect(sole).toBeVisible();
	await sole.click();

	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
	await expect(page.locator('[data-testid="queue-panel"] [aria-current="true"]')).toHaveCount(0);
	await expect(page.getByTestId('queue-clear')).toBeDisabled();
	await expect(page.getByTestId('player-next')).toBeDisabled();
	expect(await playCallsOf(page)).toBe(0);
	// Negative: the queue stays empty — nothing repopulates or reselects.
	await expect(page.getByTestId('now-playing')).not.toContainText('Demo track');
	expectDemoTraffic(traffic);
});

test('DM-B-05 each removal announces its exact outcome once through the shared live region', async ({
	page,
	baseURL
}) => {
	const traffic: Traffic = { apiCalls: [], foreign: [] };
	await openDemo(page, baseURL!, traffic);

	await expect(announcement(page)).toHaveCount(1);
	await expect(announcement(page)).toHaveAttribute('role', 'status');

	// A move first: the removal must replace any earlier message, whether or
	// not the move itself announces (top/bottom jumps may land underneath).
	await page.getByTestId('queue-move-down-demo-track').click();
	expect(await queueIds(page)).toEqual(['second-signal', 'demo-track', 'third-light']);

	const remove = removeButton(page, 'second-signal');
	await expect(remove).toBeVisible();
	await watchAnnouncements(page);
	await remove.click();

	expect(await queueIds(page)).toEqual(['demo-track', 'third-light']);
	await expect(announcement(page)).toHaveText('Second Signal removed from the queue');
	await expect(announcement(page)).not.toContainText('moved to');
	await expect
		.poll(() => announcementCommits(page), { message: 'remove announces exactly once' })
		.toHaveLength(1);
	await page.waitForTimeout(150);
	expect(await announcementCommits(page)).toHaveLength(1);
	await expect(announcement(page)).toHaveText('Second Signal removed from the queue');
	expectDemoTraffic(traffic);
});

test('DM-B-06 removing the focused middle row focuses the next surviving Remove', async ({
	page,
	baseURL
}) => {
	const traffic: Traffic = { apiCalls: [], foreign: [] };
	await openDemo(page, baseURL!, traffic);

	const remove = removeButton(page, 'second-signal');
	await expect(remove).toBeVisible();
	await remove.focus();
	await expect(remove).toBeFocused();
	await page.keyboard.press('Enter');

	expect(await queueIds(page)).toEqual(['demo-track', 'third-light']);
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	await expect(currentSelect(page)).toHaveAttribute(
		'data-testid',
		'queue-select-demo-track'
	);
	await expectUsableRemove(removeButton(page, 'third-light'));
	const identity = await activeIdentity(page);
	expect(identity.track).toBe('third-light');
	expect(identity.tag).not.toBe('BODY');
	expectDemoTraffic(traffic);
});

test('DM-B-07 removing the focused last row advances to, and focuses, the previous Remove', async ({
	page,
	baseURL
}) => {
	const traffic: Traffic = { apiCalls: [], foreign: [] };
	await openDemo(page, baseURL!, traffic);

	await page.getByTestId('queue-select-third-light').click();
	await expect(currentSelect(page)).toHaveAttribute(
		'data-testid',
		'queue-select-third-light'
	);

	const remove = removeButton(page, 'third-light');
	await expect(remove).toBeVisible();
	await remove.focus();
	await page.keyboard.press('Enter');

	expect(await queueIds(page)).toEqual(['demo-track', 'second-signal']);
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await expect(currentSelect(page)).toHaveAttribute(
		'data-testid',
		'queue-select-second-signal'
	);
	await expectUsableRemove(removeButton(page, 'second-signal'));
	const identity = await activeIdentity(page);
	expect(identity.track).toBe('second-signal');
	expect(identity.tag).not.toBe('BODY');
	expectDemoTraffic(traffic);
});

test('DM-B-08 consecutive keyboard removals stay operable until the queue empties onto its target', async ({
	page,
	baseURL
}) => {
	const traffic: Traffic = { apiCalls: [], foreign: [] };
	await openDemo(page, baseURL!, traffic);

	const first = removeButton(page, 'second-signal');
	await expect(first).toBeVisible();
	await first.focus();
	await page.keyboard.press('Enter');

	expect(await queueIds(page)).toEqual(['demo-track', 'third-light']);
	const next = removeButton(page, 'third-light');
	await expectUsableRemove(next);
	await page.keyboard.press('Enter');

	expect(await queueIds(page)).toEqual(['demo-track']);
	const last = removeButton(page, 'demo-track');
	await expectUsableRemove(last);
	await page.keyboard.press('Enter');

	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
	await expectEmptyRecovery(page);
	expectDemoTraffic(traffic);
});

test('DM-B-09 a removal that never held focus leaves focus alone', async ({ page, baseURL }) => {
	const traffic: Traffic = { apiCalls: [], foreign: [] };
	await openDemo(page, baseURL!, traffic);

	const remove = removeButton(page, 'second-signal');
	await expect(remove).toBeVisible();
	const clear = page.getByTestId('queue-clear');
	await clear.focus();
	await expect(clear).toBeFocused();
	// dispatchEvent activates without moving focus the way a pointer would.
	await remove.dispatchEvent('click');

	expect(await queueIds(page)).toEqual(['demo-track', 'third-light']);
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	await expect(clear).toBeFocused();
	expect((await activeIdentity(page)).tag).not.toBe('BODY');
	expectDemoTraffic(traffic);
});

test('DM-B-10 touch and keyboard removals work at a narrow 360px viewport', async ({
	page,
	baseURL
}) => {
	await page.setViewportSize({ width: 360, height: 800 });
	const traffic: Traffic = { apiCalls: [], foreign: [] };
	await openDemo(page, baseURL!, traffic);

	const tapped = removeButton(page, 'second-signal');
	await expect(tapped).toBeVisible();
	await expect(tapped).toBeEnabled();
	expect(await tapped.boundingBox()).not.toBeNull();
	await tapped.tap();

	expect(await queueIds(page)).toEqual(['demo-track', 'third-light']);
	await expect(announcement(page)).toHaveText('Second Signal removed from the queue');

	const remove = removeButton(page, 'third-light');
	await expect(remove).toBeVisible();
	await remove.focus();
	await page.keyboard.press('Enter');

	expect(await queueIds(page)).toEqual(['demo-track']);
	const recovered = removeButton(page, 'demo-track');
	await expectUsableRemove(recovered);
	await expect(recovered).toBeInViewport();
	expectDemoTraffic(traffic);
});

test('DM-B-11 regression: selection, moves, transport, clear and re-add keep working', async ({
	page,
	baseURL
}) => {
	const traffic: Traffic = { apiCalls: [], foreign: [] };
	await openDemo(page, baseURL!, traffic);

	await page.getByTestId('queue-select-second-signal').click();
	await expect(currentSelect(page)).toHaveAttribute(
		'data-testid',
		'queue-select-second-signal'
	);
	await page.getByTestId('player-next').click();
	await expect(currentSelect(page)).toHaveAttribute(
		'data-testid',
		'queue-select-third-light'
	);
	await expect(page.getByTestId('now-playing')).toContainText('Third Light');

	await page.getByTestId('queue-move-up-third-light').click();
	expect(await queueIds(page)).toEqual(['demo-track', 'third-light', 'second-signal']);
	await expect(currentSelect(page)).toHaveAttribute(
		'data-testid',
		'queue-select-third-light'
	);

	await page.getByTestId('queue-clear').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(0);

	await page.getByTestId('queue-add-third-light').click();
	await page.getByTestId('queue-add-demo-track').click();
	expect(await queueIds(page)).toEqual(['third-light', 'demo-track']);
	await expect(page.getByTestId('now-playing')).toContainText('Third Light');
	expectDemoTraffic(traffic);
});

test('DM-B-12 removing the same track again after a re-add announces a fresh outcome', async ({
	page,
	baseURL
}) => {
	const traffic: Traffic = { apiCalls: [], foreign: [] };
	await openDemo(page, baseURL!, traffic);

	const remove = removeButton(page, 'second-signal');
	await expect(remove).toBeVisible();
	await remove.click();
	expect(await queueIds(page)).toEqual(['demo-track', 'third-light']);
	await expect(announcement(page)).toHaveText('Second Signal removed from the queue');

	const add = page.getByTestId('queue-add-second-signal');
	await expect(add).toBeVisible();
	await add.click();
	expect(await queueIds(page)).toEqual(['demo-track', 'third-light', 'second-signal']);

	// Observe only the second removal: the identical outcome string must
	// still be a new live-region commit, or assistive tech hears nothing.
	await watchAnnouncements(page);
	const again = removeButton(page, 'second-signal');
	await expect(again).toBeVisible();
	await again.click();

	expect(await queueIds(page)).toEqual(['demo-track', 'third-light']);
	await expect(announcement(page)).toHaveText('Second Signal removed from the queue');
	await expect
		.poll(() => announcementCommits(page), {
			message:
				'repeat removal after re-add must commit a fresh announcement (next action: clear the stale removal message when a track is re-added)'
		})
		.toEqual(['Second Signal removed from the queue']);
	await page.waitForTimeout(150);
	expect(await announcementCommits(page)).toEqual(['Second Signal removed from the queue']);
	expectDemoTraffic(traffic);
});
