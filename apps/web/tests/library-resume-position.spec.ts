import { expect, test, type Page } from '@playwright/test';

/**
 * Protected Test-Author acceptance: explicit authenticated-tab library resume.
 * Builder must not edit this bank or its pure companion
 * src/lib/library/resumePosition.acceptance.test.ts.
 *
 * Browser contract (only these new public hooks are pinned):
 * - library-resume: native button, accessible name "Resume at <clock>".
 * - library-forget-position: native button named "Forget saved position".
 * - library-resume-status: polite/role=status result, explicit actions only.
 * - Storage: nntune.library.resume.v1.<existing tab auth-context>, using the
 *   shipped createResumeStore JSON shape/bounds; never the demo/global key.
 * Hidden/disabled actions are both acceptable when resume is unavailable.
 * A stale/out-of-duration or refused explicit seek must disclose unavailable,
 * never success. A successful seek must read back the requested currentTime.
 * Ordinary selection/reload/metadata never seeks to a saved position or plays.
 *
 * This fixture substitutes ONLY browser media hardware and external I/O:
 * real route, queue, storage, auth events, DOM and event listeners run unchanged.
 * No live providers, proxy, production APIs or credentials. Seeded contexts are
 * opaque synthetic tab nonces, NOT an account-identity contract.
 *
 * Canonical commands from apps/web (free port 4173; CI=1 forbids server reuse):
 *   pnpm check
 *   pnpm test
 *   CI=1 pnpm exec playwright test tests/library-resume-position.spec.ts --workers=1
 * Existing regressions: library-share-link*.spec.ts, library-cache.spec.ts,
 * player-comforts.spec.ts, player-local-state-bounds.spec.ts,
 * security-auth-transition.spec.ts, account-security-library-cache.spec.ts.
 * No new production imports: missing integration fails as an assertion receipt,
 * never a missing-module, browser-launch or timeout claimed as feature RED.
 */

const CONTEXT_KEY = 'nntune.library.auth-context.v1';
const CONTEXT = 'synthetic-resume-tab-a';
const KEY = `nntune.library.resume.v1.${CONTEXT}`;
const OTHER_KEY = 'nntune.library.resume.v1.synthetic-resume-tab-b';
const DEMO_KEY = 'nntune:resume:v1';
const ITEMS = [
	{ id: 'resume-coral', title: 'Coral Night', artist: 'Mira', streamUrl: '/test-audio/resume-coral' },
	{ id: 'resume-pine', title: 'Pine Morning', streamUrl: '/test-audio/resume-pine' }
];

type Item = { id: string; title: string; artist?: string; streamUrl?: string };
type MediaState = { time: number; duration: number; seekable: boolean; mode: 'normal' | 'throw' | 'clamp' };
type StorageCall = { op: string; key: string; value?: string; area: string };
type Listener = { element: HTMLMediaElement; type: string; callback: EventListenerOrEventListenerObject; removed: boolean };
interface Harness {
	state(element: HTMLMediaElement): MediaState;
	calls: { play: number; load: number; seeks: number[] };
	storage: StorageCall[];
	listeners: Listener[];
	late: Listener[];
	old: HTMLMediaElement | null;
	messages: string[];
	logoutRelease: (() => void) | null;
	logoutCalls: number;
}
declare global {
	interface Window { __libraryResume: Harness }
}

function receipt(id: string, expected: unknown, actual: unknown, code = 'LIBRARY_RESUME_MISSING') {
	return JSON.stringify({ case: id, expected, actual, status: 'RED', error_code: code,
		next_action: 'Implement library resume integration; keep the protected bank unchanged.' });
}

async function openLibrary(page: Page, options: {
	saved?: string | null; url?: string; items?: Item[];
	deny?: 'context' | 'resume-read' | 'resume-write';
} = {}) {
	const unexpected: string[] = [];
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(error.message));
	await page.route('**/*', async (route) => {
		const url = new URL(route.request().url());
		if (url.pathname === '/api/library') return route.fulfill({ json: options.items ?? ITEMS });
		if (url.pathname.startsWith('/test-audio/')) return route.fulfill({ status: 204 });
		if (url.pathname.startsWith('/api/') || url.pathname.startsWith('/v1/') || !['localhost', '127.0.0.1'].includes(url.hostname)) {
			unexpected.push(`${route.request().method()} ${url.pathname}`);
			return route.abort('blockedbyclient');
		}
		return route.continue();
	});
	await page.addInitScript(({ contextKey, context, key, other, demo, saved, deny }) => {
		// Seed exactly once per tab: reload must exercise persistence, not re-seeding.
		if (!sessionStorage.getItem('library-resume-fixture-seeded')) {
			sessionStorage.setItem('library-resume-fixture-seeded', 'yes');
			sessionStorage.setItem(contextKey, context);
			if (saved !== null) sessionStorage.setItem(key, saved);
			sessionStorage.setItem(other, '{"resume-coral":88}');
			sessionStorage.setItem(demo, '{"resume-coral":99}');
			sessionStorage.setItem('unrelated-resume-sentinel', 'preserve');
		}
		const states = new WeakMap<HTMLMediaElement, MediaState>();
		const harness: Harness = window.__libraryResume = {
			state(element) {
				let value = states.get(element);
				if (!value) { value = { time: 0, duration: 180, seekable: true, mode: 'normal' }; states.set(element, value); }
				return value;
			},
			calls: { play: 0, load: 0, seeks: [] }, storage: [], listeners: [], late: [], old: null, messages: [],
			logoutRelease: null, logoutCalls: 0
		};
		Object.defineProperties(HTMLMediaElement.prototype, {
			currentTime: { configurable: true, get() { return harness.state(this).time; }, set(value: number) {
				harness.calls.seeks.push(value);
				const state = harness.state(this);
				if (state.mode === 'throw') throw new DOMException('Synthetic seek refusal', 'InvalidStateError');
				state.time = state.mode === 'clamp' ? 12 : value;
			} },
			duration: { configurable: true, get() { return harness.state(this).duration; } },
			readyState: { configurable: true, get: () => 4 },
			seekable: { configurable: true, get() {
				const state = harness.state(this);
				return { length: state.seekable ? 1 : 0, start: () => 0, end: () => state.duration };
			} }
		});
		HTMLMediaElement.prototype.play = function () { harness.calls.play++; return Promise.resolve(); };
		HTMLMediaElement.prototype.load = function () { harness.calls.load++; };
		// Keep native paused/controls/rate behavior. No fake route/controller logic.
		for (const op of ['getItem', 'setItem', 'removeItem'] as const) {
			const original = Storage.prototype[op];
			Object.defineProperty(Storage.prototype, op, { configurable: true, value: function (name: string, value?: string) {
				const area = this === sessionStorage ? 'session' : 'local';
				harness.storage.push({ op, key: name, area, ...(value === undefined ? {} : { value }) });
				if ((deny === 'context' && name === contextKey) ||
					(deny === 'resume-read' && name.startsWith('nntune.library.resume.v1') && op === 'getItem') ||
					(deny === 'resume-write' && name.startsWith('nntune.library.resume.v1') && op !== 'getItem')) {
					throw new DOMException('Synthetic storage denial', 'SecurityError');
				}
				return Reflect.apply(original, this, value === undefined ? [name] : [name, value]);
			} });
		}
		const add = EventTarget.prototype.addEventListener;
		const remove = EventTarget.prototype.removeEventListener;
		EventTarget.prototype.addEventListener = function (type, callback, options) {
			if (this instanceof HTMLMediaElement && callback && ['timeupdate', 'pause', 'ended'].includes(type)) {
				harness.listeners.push({ element: this, type, callback, removed: false });
			}
			return add.call(this, type, callback, options);
		};
		EventTarget.prototype.removeEventListener = function (type, callback, options) {
			for (const listener of harness.listeners) {
				if (listener.element === this && listener.type === type && listener.callback === callback) listener.removed = true;
			}
			return remove.call(this, type, callback, options);
		};
	}, { contextKey: CONTEXT_KEY, context: CONTEXT, key: KEY, other: OTHER_KEY, demo: DEMO_KEY,
		saved: options.saved === undefined ? '{"resume-coral":42.5,"resume-pine":31}' : options.saved,
		deny: options.deny });
	await page.goto(options.url ?? '/library');
	await expect(page.getByTestId('library-item')).toHaveCount((options.items ?? ITEMS).length);
	return { unexpected, errors };
}

async function select(page: Page, item: Item = ITEMS[0], becomesCurrent = true) {
	const escaped = item.title.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
	await page.locator(`[data-testid="library-item"][data-id="${item.id}"]`)
		.getByRole('button', { name: new RegExp(`^${escaped}( — |$)`) }).click();
	await expect(page.getByTestId('audio-player')).toHaveCount(1);
	if (becomesCurrent) await expect(page.locator('[data-testid="queue-item"][aria-current="true"]'))
		.toHaveAttribute('data-track-id', item.id);
}

async function currentTime(page: Page) {
	return page.getByTestId('audio-player').evaluate((element: HTMLAudioElement) => element.currentTime);
}

async function position(page: Page, seconds: number, event = 'timeupdate') {
	await page.getByTestId('audio-player').evaluate((element: HTMLAudioElement, { seconds, event }) => {
		window.__libraryResume.state(element).time = seconds;
		element.dispatchEvent(new Event(event));
	}, { seconds, event });
}

async function raw(page: Page, key = KEY) { return page.evaluate((key) => sessionStorage.getItem(key), key); }
async function entries(page: Page) { return JSON.parse(await raw(page) ?? '{}') as Record<string, number>; }

async function controls(page: Page, id: string) {
	const resume = page.getByTestId('library-resume');
	const forget = page.getByTestId('library-forget-position');
	expect(await resume.count(), receipt(id, 'explicit Resume at <time> button', await resume.count())).toBe(1);
	expect(await forget.count(), receipt(id, 'per-current-track Forget saved position button', await forget.count())).toBe(1);
	expect(await resume.evaluate((element) => element.tagName)).toBe('BUTTON');
	expect(await forget.evaluate((element) => element.tagName)).toBe('BUTTON');
	await expect(resume).toHaveRole('button');
	await expect(resume).toHaveAccessibleName(/resume at.*\d:\d{2}/i);
	await expect(forget).toHaveRole('button');
	await expect(forget).toHaveAccessibleName(/forget saved position/i);
	return { resume, forget };
}

async function inertState(page: Page) {
	return page.evaluate(({ demo, other, current }) => {
		const audio = document.querySelector<HTMLAudioElement>('[data-testid="audio-player"]');
		return { play: window.__libraryResume.calls.play, load: window.__libraryResume.calls.load,
			queue: Array.from(document.querySelectorAll('[data-testid="queue-item"]')).map((row) => [row.getAttribute('data-track-id'), row.getAttribute('aria-current')]),
			src: audio?.getAttribute('src'), paused: audio?.paused, controls: audio?.controls, rate: audio?.playbackRate,
			demo: sessionStorage.getItem(demo), other: sessionStorage.getItem(other),
			local: Object.fromEntries(Object.keys(localStorage).map((key) => [key, localStorage.getItem(key)])),
			session: Object.fromEntries(Object.keys(sessionStorage).filter((key) => key !== current)
				.map((key) => [key, sessionStorage.getItem(key)])),
			unrelated: sessionStorage.getItem('unrelated-resume-sentinel') };
	}, { demo: DEMO_KEY, other: OTHER_KEY, current: KEY });
}

async function noAvailableResume(page: Page) {
	const resume = page.getByTestId('library-resume');
	if (await resume.isVisible()) await expect(resume).toBeDisabled();
}

async function navigate(page: Page, path: string) {
	await page.evaluate((path) => {
		const anchor = document.createElement('a'); anchor.id = 'resume-test-navigation';
		anchor.href = path; anchor.textContent = 'Synthetic route navigation'; document.body.append(anchor);
	}, path);
	await page.locator('#resume-test-navigation').click();
	await expect(page).toHaveURL(new RegExp(`${path.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}$`));
}

async function retainCallbacks(page: Page) {
	await page.evaluate(() => {
		const h = window.__libraryResume;
		h.old = document.querySelector<HTMLAudioElement>('[data-testid="audio-player"]');
		h.late = h.listeners.filter((listener) => listener.element === h.old && ['pause', 'timeupdate'].includes(listener.type));
	});
	expect(await page.evaluate(() => window.__libraryResume.late.length), 'position listener capture must be nonempty').toBeGreaterThan(0);
}

async function fireLate(page: Page) {
	await page.evaluate(() => {
		for (const { element, type, callback } of window.__libraryResume.late) {
			const event = new Event(type); Object.defineProperty(event, 'target', { value: element });
			if (typeof callback === 'function') callback.call(element, event); else callback.handleEvent(event);
		}
	});
}

test('LIBRESUME-01 selection is inert; explicit resume seeks once without autoplay or unrelated effects', async ({ page }) => {
	const io = await openLibrary(page);
	await select(page);
	expect(await currentTime(page)).toBe(0);
	const { resume } = await controls(page, 'LIBRESUME-01');
	await expect(resume).toHaveAccessibleName(/resume at.*0:42/i);
	const before = await inertState(page);
	expect(before.paused).toBe(true); expect(before.controls).toBe(true); expect(before.play).toBe(0);
	const seeksBefore = await page.evaluate(() => window.__libraryResume.calls.seeks.length);
	await resume.focus(); await resume.press('Enter');
	expect(await currentTime(page), receipt('LIBRESUME-01', 42.5, await currentTime(page))).toBe(42.5);
	await page.getByTestId('audio-player').evaluate((element) => {
		for (const event of ['loadedmetadata', 'durationchange', 'canplay']) element.dispatchEvent(new Event(event));
	});
	expect(await page.evaluate((start) => window.__libraryResume.calls.seeks.slice(start), seeksBefore)).toEqual([42.5]);
	await expect(resume).toBeFocused();
	expect(await inertState(page)).toEqual(before);
	expect(io.unexpected).toEqual([]); expect(io.errors).toEqual([]);
	await expect(page.getByTestId('library-resume-status')).toContainText(/resum|saved position/i);
});

test('LIBRESUME-02 pause flush survives reload in the same authenticated tab, never auto-resumes', async ({ page }) => {
	await openLibrary(page); await select(page);
	await position(page, 37.25, 'pause');
	expect((await entries(page))['resume-coral'], receipt('LIBRESUME-02', 37.25, await raw(page))).toBe(37.25);
	await page.reload(); await expect(page.getByTestId('library-item')).toHaveCount(2); await select(page);
	expect(await currentTime(page)).toBe(0);
	const { resume } = await controls(page, 'LIBRESUME-02');
	await expect(resume).toHaveAccessibleName(/resume at.*0:37/i);
	await resume.click(); expect(await currentTime(page)).toBe(37.25);
	expect((await inertState(page)).play).toBe(0);
});

test('LIBRESUME-03 demo and other-tab keys are neither read nor used as an unscoped fallback', async ({ page }) => {
	await openLibrary(page, { saved: null }); await select(page);
	await noAvailableResume(page); expect(await currentTime(page)).toBe(0);
	await position(page, 18, 'pause');
	expect((await entries(page))['resume-coral'], receipt('LIBRESUME-03', 18, await raw(page))).toBe(18);
	const forbiddenReads = await page.evaluate(({ demo, other }) => window.__libraryResume.storage
		.filter((call) => call.op === 'getItem' && [demo, other].includes(call.key)), { demo: DEMO_KEY, other: OTHER_KEY });
	expect(forbiddenReads).toEqual([]);
	expect(await raw(page, DEMO_KEY)).toBe('{"resume-coral":99}');
	expect(await raw(page, OTHER_KEY)).toBe('{"resume-coral":88}');
});

test('LIBRESUME-04 timeupdate bursts coalesce; pause flushes the last valid current position immediately', async ({ page }) => {
	await page.clock.install(); await openLibrary(page, { saved: null }); await select(page);
	await page.getByTestId('audio-player').evaluate((element: HTMLAudioElement) => {
		window.__libraryResume.storage.length = 0;
		for (let value = 1; value <= 20; value++) {
			window.__libraryResume.state(element).time = value; element.dispatchEvent(new Event('timeupdate'));
		}
	});
	const writes = () => page.evaluate((key) => window.__libraryResume.storage.filter((call) => call.key === key && call.op === 'setItem'), KEY);
	expect((await writes()).length).toBeLessThan(20);
	await page.clock.runFor(10_000);
	expect((await entries(page))['resume-coral'], receipt('LIBRESUME-04', 'coalesced timeupdate persists 20 before pause', await raw(page))).toBe(20);
	expect((await writes()).length).toBeGreaterThan(0);
	expect((await writes()).length).toBeLessThan(20);
	await position(page, 21.75, 'pause');
	expect((await entries(page))['resume-coral'], receipt('LIBRESUME-04', 21.75, await raw(page))).toBe(21.75);
	await page.clock.runFor(10_000);
	expect((await entries(page))['resume-coral']).toBe(21.75);
	expect((await writes()).length).toBeLessThan(20);
});

test('LIBRESUME-05 route teardown flushes before disposal and removes position listeners', async ({ page }) => {
	await openLibrary(page, { saved: null }); await select(page); await retainCallbacks(page);
	await position(page, 55.5, 'seeking'); // No timeupdate/pause: teardown owns the flush.
	await navigate(page, '/');
	expect((await entries(page))['resume-coral'], receipt('LIBRESUME-05', 55.5, await raw(page))).toBe(55.5);
	const before = await raw(page); await fireLate(page);
	expect(await raw(page)).toBe(before);
	const remaining = await page.evaluate(() => window.__libraryResume.late.filter((listener) => !listener.removed).length);
	expect(remaining).toBe(0);
});

test('LIBRESUME-06 natural end clears only the current entry and does not restore its terminal position', async ({ page }) => {
	await openLibrary(page); await select(page);
	await position(page, 180, 'ended');
	expect(Object.hasOwn(await entries(page), 'resume-coral'), receipt('LIBRESUME-06', 'current entry removed', await raw(page))).toBe(false);
	expect((await entries(page))['resume-pine']).toBe(31);
});

test('LIBRESUME-07 Forget is per-track, repeatable, focus-preserving and never resets playback', async ({ page }) => {
	await page.clock.install(); const io = await openLibrary(page); await select(page);
	const { forget } = await controls(page, 'LIBRESUME-07');
	await position(page, 11); // A pending coalesced save must not resurrect Forget.
	const before = await inertState(page);
	await forget.focus();
	// Two explicit activations also test idempotent clearing if the control is removed.
	await forget.evaluate((button: HTMLButtonElement) => { button.click(); button.click(); });
	await page.clock.runFor(10_000);
	expect(await currentTime(page)).toBe(11);
	expect(Object.hasOwn(await entries(page), 'resume-coral')).toBe(false);
	expect((await entries(page))['resume-pine']).toBe(31);
	expect(await inertState(page)).toEqual(before);
	if (await forget.isVisible()) await expect(forget).toBeFocused();
	else expect(await page.evaluate(() => document.activeElement !== document.body)).toBe(true);
	await noAvailableResume(page); expect(io.unexpected).toEqual([]); expect(io.errors).toEqual([]);
});

for (const [label, rawValue] of [
	['zero', '{"resume-coral":0}'], ['negative', '{"resume-coral":-1}'],
	['overflow', '{"resume-coral":1e999}'], ['string', '{"resume-coral":"42"}'],
	['corrupt', '{broken'], ['array', '[42]'],
	['entry-cap', JSON.stringify(Object.fromEntries(Array.from({ length: 257 }, (_, i) => [i === 0 ? 'resume-coral' : `entry-${i}`, 5])))],
	['byte-cap', '{"resume-coral":42}' + ' '.repeat(65_536)]
]) {
	test(`LIBRESUME-08 ${label} storage cannot offer or automatically apply a saved seek`, async ({ page }) => {
		const io = await openLibrary(page, { saved: rawValue }); await select(page);
		await noAvailableResume(page); expect(await currentTime(page)).toBe(0);
		expect((await inertState(page)).play).toBe(0); expect(io.errors).toEqual([]);
	});
}

test('LIBRESUME-09 corrupt saved state recovers on valid pause without touching unrelated keys', async ({ page }) => {
	await openLibrary(page, { saved: '{broken' }); await select(page);
	const before = await inertState(page); await position(page, 8.5, 'pause');
	expect(await entries(page), receipt('LIBRESUME-09', { 'resume-coral': 8.5 }, await raw(page))).toEqual({ 'resume-coral': 8.5 });
	expect(await inertState(page)).toEqual(before);
});

for (const value of [NaN, Infinity, -Infinity, -1, 181]) {
	test(`LIBRESUME-10 invalid current position ${value} never overwrites a valid saved entry`, async ({ page }) => {
		await openLibrary(page); await select(page);
		await position(page, value, 'timeupdate'); await position(page, value, 'pause');
		expect((await entries(page))['resume-coral']).toBe(42.5);
	});
}

for (const duration of [NaN, Infinity, 0]) {
	test(`LIBRESUME-11 unusable duration ${duration} offers no actionable resume`, async ({ page }) => {
		await openLibrary(page); await select(page);
		await page.getByTestId('audio-player').evaluate((element: HTMLAudioElement, duration) => {
			window.__libraryResume.state(element).duration = duration; element.dispatchEvent(new Event('durationchange'));
		}, duration);
		await noAvailableResume(page); expect(await currentTime(page)).toBe(0);
	});
}

test('LIBRESUME-12 a saved position beyond the current duration is disclosed as unavailable, not clamped-success', async ({ page }) => {
	await openLibrary(page, { saved: '{"resume-coral":181}' }); await select(page);
	const resume = page.getByTestId('library-resume');
	if (await resume.isVisible() && await resume.isEnabled()) await resume.click();
	expect(await currentTime(page)).toBe(0);
	const text = await page.locator('main').innerText();
	expect(text, receipt('LIBRESUME-12', 'stale/unavailable saved-position disclosure', text, 'STALE_RESUME_NOT_DISCLOSED'))
		.toMatch(/(?:saved position|resume).*(?:unavailable|stale|no longer)|(?:unavailable|stale).*saved position/i);
});

test('LIBRESUME-13 a current element without seekable ranges cannot activate Resume', async ({ page }) => {
	await openLibrary(page); await select(page);
	await page.getByTestId('audio-player').evaluate((element: HTMLAudioElement) => {
		window.__libraryResume.state(element).seekable = false; element.dispatchEvent(new Event('durationchange'));
	});
	await noAvailableResume(page); expect(await currentTime(page)).toBe(0);
});

for (const mode of ['throw', 'clamp'] as const) {
	test(`LIBRESUME-14 ${mode} seek checks actual currentTime and announces refusal instead of success`, async ({ page }) => {
		const io = await openLibrary(page); await select(page);
		const { resume } = await controls(page, 'LIBRESUME-14');
		await page.getByTestId('audio-player').evaluate((element: HTMLAudioElement, mode) => { window.__libraryResume.state(element).mode = mode; }, mode);
		const before = await inertState(page); await resume.focus(); await resume.press('Space');
		expect(await currentTime(page)).toBe(mode === 'throw' ? 0 : 12);
		const status = page.getByTestId('library-resume-status');
		await expect(status).toContainText(/unavailable|could not|unable|cannot/i);
		await expect(status).not.toContainText(/resumed|restored|success/i);
		await expect(resume).toBeFocused(); expect(await inertState(page)).toEqual(before); expect(io.errors).toEqual([]);
	});
}

test('LIBRESUME-15 inbound timestamp takes precedence through late metadata and duration callbacks', async ({ page }) => {
	await openLibrary(page, { url: '/library?track=resume-coral&t=9', saved: '{"resume-coral":75}' });
	await expect(page.getByTestId('audio-player')).toHaveCount(1);
	await expect.poll(() => currentTime(page)).toBe(9);
	await page.getByTestId('audio-player').evaluate((element) => {
		for (const name of ['loadedmetadata', 'durationchange', 'canplay']) element.dispatchEvent(new Event(name));
	});
	expect(await currentTime(page)).toBe(9); expect((await inertState(page)).play).toBe(0);
	await position(page, 10, 'pause');
	expect((await entries(page))['resume-coral'], receipt('LIBRESUME-15', 10, await raw(page))).toBe(10);
});

test('LIBRESUME-16 replacing current track/source fences retained old callbacks', async ({ page }) => {
	await page.clock.install(); await openLibrary(page); await select(page);
	await position(page, 22, 'pause');
	expect((await entries(page))['resume-coral'], receipt('LIBRESUME-16', 22, await raw(page))).toBe(22);
	await retainCallbacks(page);
	await select(page, ITEMS[1], false); // Selecting appends; Next explicitly changes the current source.
	await page.getByTestId('player-next').click();
	await expect(page.locator('[data-testid="queue-item"][aria-current="true"]')).toHaveAttribute('data-track-id', 'resume-pine');
	const before = await raw(page);
	await page.evaluate(() => {
		const h = window.__libraryResume;
		if (h.old) h.state(h.old).time = 67.25;
		h.storage.length = 0;
	});
	await fireLate(page); await page.clock.runFor(10_000);
	expect(await raw(page)).toBe(before);
	expect(await page.evaluate((key) => window.__libraryResume.storage.filter((call) =>
		call.key === key && call.op !== 'getItem'), KEY)).toEqual([]);
	const { resume } = await controls(page, 'LIBRESUME-16'); await resume.click();
	expect(await currentTime(page)).toBe(31);
});

test('LIBRESUME-17 cross-tab auth transition clears outgoing data before revocation and cannot be resurrected', async ({ page }) => {
	await page.clock.install(); await openLibrary(page); await select(page); await retainCallbacks(page);
	await position(page, 17);
	await page.evaluate(() => { window.__libraryResume.storage.length = 0; });
	const other = await page.context().newPage(); await other.goto('/login');
	await other.evaluate(() => { localStorage.setItem('nntune.library.auth-transition.v1', 'synthetic-transition'); localStorage.removeItem('nntune.library.auth-transition.v1'); });
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	expect(await raw(page), receipt('LIBRESUME-17', 'outgoing saved positions removed', await raw(page), 'AUTH_SCOPE_NOT_CLEARED')).toBeNull();
	await assertClearOrder(page, 'LIBRESUME-17');
	await fireLate(page); await page.clock.runFor(10_000); await navigate(page, '/');
	expect(await raw(page)).toBeNull(); expect(await raw(page, CONTEXT_KEY)).toBeNull();
	expect(await raw(page, OTHER_KEY)).toBe('{"resume-coral":88}'); expect(await raw(page, DEMO_KEY)).toBe('{"resume-coral":99}');
	await other.close();
});

async function assertClearOrder(page: Page, id: string) {
	const calls = await page.evaluate(() => window.__libraryResume.storage);
	const cleared = calls.findIndex((call) => call.key === KEY && (call.op === 'removeItem' || (call.op === 'setItem' && call.value === '{}')));
	const revoked = calls.findIndex((call) => call.key === CONTEXT_KEY && call.op === 'removeItem');
	expect(cleared, receipt(id, 'clear outgoing resume before context revoke', calls, 'AUTH_CLEAR_ORDER')).toBeGreaterThanOrEqual(0);
	expect(revoked).toBeGreaterThan(cleared);
}

for (const status of [401, 403]) {
	test(`LIBRESUME-18 ${status} refresh refusal clears outgoing scope and ignores late teardown saves`, async ({ page }) => {
		await page.clock.install(); await openLibrary(page); await select(page); await retainCallbacks(page); await position(page, 19);
		await page.route('**/api/library', (route) => route.fulfill({ status, json: { error: 'synthetic refusal' } }));
		await page.evaluate(() => { window.__libraryResume.storage.length = 0; });
		await page.getByRole('button', { name: 'Refresh', exact: true }).click();
		await expect(page.getByRole('alert')).toContainText(/signed out|do not have access/i);
		expect(await raw(page), receipt('LIBRESUME-18', null, await raw(page), 'REFUSAL_SCOPE_NOT_CLEARED')).toBeNull();
		await assertClearOrder(page, 'LIBRESUME-18');
		await fireLate(page); await page.clock.runFor(10_000); await navigate(page, '/');
		expect(await raw(page)).toBeNull(); expect(await raw(page, CONTEXT_KEY)).toBeNull();
	});
}

for (const deny of ['context', 'resume-read', 'resume-write'] as const) {
	test(`LIBRESUME-19 ${deny} storage denial fails soft without local/demo fallback`, async ({ page }) => {
		const io = await openLibrary(page, { saved: null, deny }); await select(page); await position(page, 14, 'pause');
		const fallback = await page.evaluate((demo) => window.__libraryResume.storage.filter((call) =>
			(call.area === 'local' && call.key.startsWith('nntune.library.resume')) || call.key === demo), DEMO_KEY);
		expect(fallback).toEqual([]);
		expect(await currentTime(page)).toBe(14); expect((await inertState(page)).play).toBe(0);
		expect(io.errors).toEqual([]); expect(io.unexpected).toEqual([]);
	});
}

test('LIBRESUME-20 opaque prototype-like track IDs save and Forget as own keys only', async ({ page }) => {
	const items = [{ id: '__proto__', title: 'Opaque Prototype', streamUrl: '/test-audio/opaque' }, { id: 'constructor', title: 'Opaque Constructor', streamUrl: '/test-audio/constructor' }];
	await openLibrary(page, { items, saved: '{"__proto__":23,"constructor":34}' }); await select(page, items[0]);
	const { resume, forget } = await controls(page, 'LIBRESUME-20'); await resume.click(); expect(await currentTime(page)).toBe(23);
	await position(page, 24.5, 'pause'); expect(Object.entries(await entries(page))).toEqual([['__proto__', 24.5], ['constructor', 34]]);
	await forget.click(); expect(await entries(page)).toEqual({ constructor: 34 });
	expect(await page.evaluate(() => Object.getPrototypeOf({}) === Object.prototype && !Object.hasOwn(Object.prototype, 'polluted'))).toBe(true);
});

test('LIBRESUME-21 background events are silent; one explicit result is politely announced exactly once', async ({ page }) => {
	await page.clock.install(); await openLibrary(page); await select(page);
	const { resume } = await controls(page, 'LIBRESUME-21');
	const status = page.getByTestId('library-resume-status');
	expect(await status.innerText()).toBe('');
	await status.evaluate((element) => {
		new MutationObserver(() => {
			const text = element.textContent?.trim(); if (text) window.__libraryResume.messages.push(text);
		}).observe(element, { childList: true, subtree: true, characterData: true });
	});
	await position(page, 7); await page.clock.runFor(10_000);
	expect(await status.innerText()).toBe('');
	await resume.focus(); await resume.press('Enter'); await expect(status).not.toBeEmpty();
	await page.getByTestId('audio-player').evaluate((element) => {
		for (const name of ['loadedmetadata', 'durationchange', 'timeupdate', 'seeked']) element.dispatchEvent(new Event(name));
	});
	await page.clock.runFor(10_000);
	expect(await page.evaluate(() => window.__libraryResume.messages)).toHaveLength(1);
	const live = await status.getAttribute('aria-live');
	expect(live === 'polite' || await status.getAttribute('role') === 'status').toBe(true);
	await expect(resume).toBeFocused();
});

for (const colorScheme of ['light', 'dark'] as const) {
	test(`LIBRESUME-22 ${colorScheme} narrow touch layout keeps labeled actions reachable without overlap`, async ({ page }) => {
		await page.setViewportSize({ width: 320, height: 720 }); await page.emulateMedia({ colorScheme });
		await openLibrary(page); await select(page); const { resume, forget } = await controls(page, 'LIBRESUME-22');
		await resume.scrollIntoViewIfNeeded(); await page.keyboard.press('Tab'); await resume.focus();
		await expect(resume).toBeFocused();
		expect(await resume.evaluate((element) => element.matches(':focus-visible'))).toBe(true);
		const focus = await resume.evaluate((element) => { const style = getComputedStyle(element); return [style.outlineStyle, style.outlineWidth, style.boxShadow]; });
		expect((focus[0] !== 'none' && parseFloat(focus[1]) > 0) || focus[2] !== 'none').toBe(true);
		for (const control of [resume, forget]) {
			await control.scrollIntoViewIfNeeded();
			const box = await control.boundingBox(); expect(box).not.toBeNull();
			expect(box!.x).toBeGreaterThanOrEqual(0); expect(box!.x + box!.width).toBeLessThanOrEqual(320);
			expect(box!.width).toBeGreaterThanOrEqual(24); expect(box!.height).toBeGreaterThanOrEqual(24);
		}
		const boxes = await Promise.all([resume.boundingBox(), forget.boundingBox()]);
		expect(boxes[0]!.x + boxes[0]!.width <= boxes[1]!.x || boxes[1]!.x + boxes[1]!.width <= boxes[0]!.x ||
			boxes[0]!.y + boxes[0]!.height <= boxes[1]!.y || boxes[1]!.y + boxes[1]!.height <= boxes[0]!.y).toBe(true);
		await resume.click(); expect(await currentTime(page)).toBe(42.5);
	});
}

test('LIBRESUME-23 late same-window logout clears the mounted replacement library before context revocation', async ({ page }) => {
	await page.clock.install(); const io = await openLibrary(page);
	await page.route('**/v1/vault', (route) => route.fulfill({ json: {} }));
	await page.evaluate(() => {
		const nativeFetch = window.fetch.bind(window);
		window.fetch = (input, init) => {
			const request = new Request(input, init);
			if (new URL(request.url).pathname !== '/v1/session/logout') return nativeFetch(input, init);
			window.__libraryResume.logoutCalls++;
			return new Promise<Response>((resolve) => {
				window.__libraryResume.logoutRelease = () => resolve(new Response('{"ok":true}', {
					status: 200, headers: { 'content-type': 'application/json' }
				}));
			});
		};
	});
	await navigate(page, '/account/security');
	await page.getByRole('button', { name: /^sign out$/i }).click();
	await expect.poll(() => page.evaluate(() => window.__libraryResume.logoutCalls)).toBe(1);
	await page.locator('#resume-test-navigation').evaluate((element) => element.remove());
	await navigate(page, '/library'); await expect(page.getByTestId('library-item')).toHaveCount(2);
	await select(page); await retainCallbacks(page); await position(page, 17);
	await page.evaluate(() => { window.__libraryResume.storage.length = 0; window.__libraryResume.logoutRelease!(); });
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	expect(await raw(page), receipt('LIBRESUME-23', null, await raw(page), 'LOCAL_AUTH_SCOPE_NOT_CLEARED')).toBeNull();
	await assertClearOrder(page, 'LIBRESUME-23');
	await fireLate(page); await page.clock.runFor(10_000);
	expect(await raw(page)).toBeNull(); expect(await raw(page, CONTEXT_KEY)).toBeNull();
	expect(new URL(page.url()).pathname).toBe('/library'); expect(io.errors).toEqual([]); expect(io.unexpected).toEqual([]);
});

test('LIBRESUME-33 select-only rows without a native media element never offer a saved seek', async ({ page }) => {
	await openLibrary(page, { items: [{ id: 'resume-coral', title: 'Coral Night' }] });
	await page.locator('[data-testid="library-item"][data-id="resume-coral"]')
		.getByRole('button', { name: /^Coral Night$/ }).click();
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
	await noAvailableResume(page);
	expect((await entries(page))['resume-coral']).toBe(42.5);
});
