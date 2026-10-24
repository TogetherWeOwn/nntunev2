import { expect, test, type Page } from '@playwright/test';

/**
 * Acceptance: bounded local player-state decoding on the demo player (browser half).
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build half continues
 * this branch and must make these tests pass WITHOUT editing this file; a
 * wrong test comes back to the acceptance card for this file instead.
 *
 * Surface: `/demo/player`, the only surface with real media events. Media
 * loading is held pending (route-held, never fulfilled) and `play()` is
 * stubbed to resolve, so flows are driven by synthetic media events with no
 * network beyond the local preview server (the radio-continue-spec pattern).
 *
 * What's pinned: oversized but otherwise-valid persisted payloads fail closed
 * at startup (no resume restore, no history rows) while the player stays
 * usable; hostile resume keys never mutate prototypes and never disturb
 * ordinary per-track resume, speed, or queue behavior; a blocked
 * `localStorage` getter combined with oversized session data still mounts;
 * startup flows make zero backend calls.
 *
 * Locked decisions (so the builder never guesses):
 * - The serialized bound is 65_536 chars measured on the raw stored string.
 *   Seeded payloads below assert their own oversize (`length > 65_536`) as a
 *   fixture guard, so a silently shrunk fixture fails loudly.
 * - Fail-closed means: resume restores nothing (element stays at 0), history
 *   shows the honest empty state, and the next play re-records normally.
 * - Rendering filters unknown ids (stored but not shown); seeded payloads
 *   therefore lead with real demo ids so the RED state is visible rows, not
 *   an already-empty list that would pass vacuously.
 * - Mount-time restore settles within the post-mount wait: the buggy value
 *   lands during mount (long before the read), while fixed code never seeks,
 *   so a single settled read is deterministic in both states.
 * - Prototype checks are controls: they pass before and after the build and
 *   pin "no worse than today" for hostile keys.
 *
 * Out of scope (not pinned): favorites decoding, queue/speed/volume store
 * internals, auth scopes, telemetry, cross-tab sync, exact relative-label
 * wording, reduced-motion internals.
 *
 * CASE INDEX (stable IDs; LSB-U-* live in the unit acceptance file):
 * LSB-B-01 oversized resume refuses restore, LSB-B-02 oversized history
 * shows the empty state and recovers, LSB-B-03 blocked getter plus oversized
 * history still mounts with zero backend calls, LSB-B-04 hostile resume keys
 * leave ordinary resume, speed and prototypes intact.
 */

const RESUME_KEY = 'nntune:resume:v1';
const RECENT_KEY = 'nntune:demo-recently-played:v1';
const MAX_SERIALIZED_CHARS = 65_536;

async function openPlayer(page: Page, url = '/demo/player'): Promise<void> {
	await page.route('**/api/demo/audio**', () => {});
	await page.addInitScript(() => {
		HTMLMediaElement.prototype.play = function () {
			return Promise.resolve();
		};
		HTMLMediaElement.prototype.pause = function () {};
	});
	await page.goto(url);
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
}

async function currentTime(page: Page): Promise<number> {
	return page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => el.currentTime);
}

async function recentIds(page: Page): Promise<string[]> {
	return page.getByTestId('recent-item').evaluateAll((items) =>
		items.map((item) => item.getAttribute('data-track-id') ?? '')
	);
}

async function prototypeIntact(page: Page): Promise<boolean> {
	return page.evaluate(() => {
		if (Object.getPrototypeOf({}) !== Object.prototype) return false;
		if (typeof ({} as Record<string, unknown>)['toString'] !== 'function') return false;
		if ('polluted' in {}) return false;
		return true;
	});
}

/** Valid resume map over the serialized cap, restoring 31.25 iff parsed. */
function oversizedResumeRaw(): string {
	const entries: Record<string, number> = { 'demo-track': 31.25 };
	for (let i = 0; i < 5000; i += 1) entries[`pad-${i}`] = i;
	const raw = JSON.stringify(entries);
	if (raw.length <= MAX_SERIALIZED_CHARS) throw new Error('resume fixture is not oversized');
	return raw;
}

/** Valid history array over the serialized cap, demo ids first. */
function oversizedRecentRaw(): string {
	const entries = [
		{ trackId: 'demo-track', playedAt: 3000 },
		{ trackId: 'second-signal', playedAt: 2999 },
		{ trackId: 'third-light', playedAt: 2998 },
		...Array.from({ length: 4997 }, (_, i) => ({ trackId: `pad-${i}`, playedAt: i }))
	];
	const raw = JSON.stringify(entries);
	if (raw.length <= MAX_SERIALIZED_CHARS) throw new Error('recent fixture is not oversized');
	return raw;
}

test('LSB-B-01 oversized resume payload restores nothing and the player stays usable', async ({
	page
}) => {
	const raw = oversizedResumeRaw();
	expect(raw.length).toBeGreaterThan(MAX_SERIALIZED_CHARS);
	const backendCalls: string[] = [];
	page.on('request', (request) => {
		if (['fetch', 'xhr'].includes(request.resourceType()) || request.method() !== 'GET') {
			backendCalls.push(`${request.method()} ${request.url()}`);
		}
	});
	await page.route('**/api/demo/audio**', () => {});
	await page.addInitScript(
		({ key, value }) => {
			localStorage.setItem(key, value);
		},
		{ key: RESUME_KEY, value: raw }
	);
	await page.addInitScript(() => {
		HTMLMediaElement.prototype.play = function () {
			return Promise.resolve();
		};
		HTMLMediaElement.prototype.pause = function () {};
	});
	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);

	// Mount-time restore has long settled: fixed code never seeks.
	await page.waitForTimeout(750);
	expect(await currentTime(page)).toBe(0);

	// Usable startup: the queue is intact (no repeated ids from stale data),
	// playback starts, and prototypes are untouched.
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	await page.getByTestId('queue-select-demo-track').click();
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await page.waitForTimeout(250);
	expect(backendCalls).toEqual([]);
	expect(await prototypeIntact(page)).toBe(true);
});

test('LSB-B-02 oversized history shows the empty state and recovers on play', async ({
	page
}) => {
	const raw = oversizedRecentRaw();
	expect(raw.length).toBeGreaterThan(MAX_SERIALIZED_CHARS);
	await page.route('**/api/demo/audio**', () => {});
	await page.addInitScript(
		({ key, value }) => {
			sessionStorage.setItem(key, value);
		},
		{ key: RECENT_KEY, value: raw }
	);
	await page.addInitScript(() => {
		HTMLMediaElement.prototype.play = function () {
			return Promise.resolve();
		};
		HTMLMediaElement.prototype.pause = function () {};
	});
	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);

	await expect(page.getByTestId('recently-played-empty')).toBeVisible();
	await expect(page.getByTestId('recent-item')).toHaveCount(0);

	// The store recovers on the next completed play.
	await page.getByTestId('queue-select-demo-track').click();
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await page.getByTestId('demo-audio').evaluate((el) => {
		Object.defineProperty(el, 'paused', { value: true, configurable: true });
		Object.defineProperty(el, 'ended', { value: true, configurable: true });
		el.dispatchEvent(new Event('pause'));
		el.dispatchEvent(new Event('ended'));
	});
	expect(await recentIds(page)).toEqual(['demo-track']);
});

test('LSB-B-03 blocked storage getter plus oversized history still mounts cleanly', async ({
	page
}) => {
	const raw = oversizedRecentRaw();
	expect(raw.length).toBeGreaterThan(MAX_SERIALIZED_CHARS);
	const backendCalls: string[] = [];
	page.on('request', (request) => {
		if (['fetch', 'xhr'].includes(request.resourceType()) || request.method() !== 'GET') {
			backendCalls.push(`${request.method()} ${request.url()}`);
		}
	});
	await page.route('**/api/demo/audio**', () => {});
	await page.addInitScript(
		({ key, value }) => {
			sessionStorage.setItem(key, value);
			Object.defineProperty(window, 'localStorage', {
				configurable: true,
				get() {
					throw new DOMException('Storage access blocked', 'SecurityError');
				}
			});
			HTMLMediaElement.prototype.play = function () {
				return Promise.resolve();
			};
			HTMLMediaElement.prototype.pause = function () {};
		},
		{ key: RECENT_KEY, value: raw }
	);

	await page.goto('/demo/player?track=second-signal&t=90');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await expect.poll(() => currentTime(page)).toBe(90);

	await expect(page.getByTestId('recently-played-empty')).toBeVisible();
	await expect(page.getByTestId('recent-item')).toHaveCount(0);

	await page.waitForTimeout(250);
	expect(backendCalls).toEqual([]);
	expect(await prototypeIntact(page)).toBe(true);
});

test('LSB-B-04 hostile resume keys leave ordinary resume, speed and prototypes intact', async ({
	page
}) => {
	await page.route('**/api/demo/audio**', () => {});
	await page.addInitScript(() => {
		localStorage.setItem(
			'nntune:resume:v1',
			'{"demo-track":22,"__proto__":42.5,"toString":"x","constructor":-5,"hasOwnProperty":null}'
		);
		HTMLMediaElement.prototype.play = function () {
			return Promise.resolve();
		};
		HTMLMediaElement.prototype.pause = function () {};
	});
	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);

	// Ordinary per-track resume restores alongside the hostile siblings.
	await expect.poll(() => currentTime(page)).toBe(22);

	// Speed comforts keep working and prototypes are untouched.
	await page.getByTestId('playback-speed').selectOption('1.5');
	await expect(page.getByTestId('playback-speed')).toHaveValue('1.5');
	expect(await prototypeIntact(page)).toBe(true);
});
