import { expect, test, type Page } from '@playwright/test';

/**
 * Test-Author owned RED acceptance for LIBREPEAT: advance at end of track with
 * an explicit repeat Off/One/Queue control on `/library`. The Builder wires
 * `/library` to satisfy this file plus
 * `src/lib/library/libraryRepeatEndPolicy.acceptance.test.ts` and must not
 * edit either file.
 *
 * LOCKED PAGE CONTRACT (new hooks on `/library`; existing rows, queue,
 * favorites, order, share and filter hooks are unchanged):
 * - `player-repeat`: one NATIVE <select>, accessible name /repeat/i, values
 *   off/one/queue with labels Off/One/Queue, default Off. Keyboard reachable
 *   with visible focus; selecting keeps focus and never steals it.
 * - `repeat-status`: one shared role=status region reading "Repeat: Off/One/
 *   Queue" for the current mode. Updates on selection without focus theft.
 * - The mode persists across reload (local-state-bounds pattern, same as other
 *   player prefs); corrupt/overlong/wrong-type stored values fall back to Off.
 * - `audio-player` (existing, rendered when the current track carries a usable
 *   streamUrl) handles `ended` through the existing `naturalEndPolicy` core:
 *   Off advances to the next queued track then stops truthfully at queue end;
 *   One replays the current track from zero; Queue loops to the first track at
 *   queue end. `play` starts from zero; `stop`/`ignore` start nothing. Never
 *   autoplays from an empty queue, never fabricates a next track, never sets a
 *   native `loop` attribute (audio.loop stays false, no loop= markup).
 * - Manual pause and stale/detached `ended` stay inert (no advance, no play).
 *   Explicit Next stays clamped at the tail under every mode. Auth teardown
 *   clears pending end handling without advancing into the next session. A
 *   fired sleep timer stops playback with no advance after it (the library has
 *   no sleep UI; the browser pin is stop-terminal behavior while the pure
 *   vectors pin sleepComplete→stop).
 * - End-of-track advance changes only the selection (now-playing identity +
 *   audio src + play-from-zero): queue order, favorites, subset selection,
 *   find scope/query, library order/view, queue filter and media state beyond
 *   the intended play are untouched. Share status/fallback stay clear, the
 *   clipboard is never touched, and no resume/share seek fires implicitly.
 * - One concise (<140 chars) polite queue-announcement per automatic advance,
 *   naming the new track; silent transitions (stop/ignore) leave the
 *   announcement empty and never chatter.
 *
 * PROTECTED (Test-Author owned; Builder must not edit):
 * - `apps/web/tests/library-repeat-end-policy.spec.ts` (this file)
 * - `apps/web/src/lib/library/libraryRepeatEndPolicy.acceptance.test.ts`
 * BUILDER-OWNED implementation surface:
 * - `apps/web/src/routes/library/+page.svelte` (only)
 * - Existing queue/resume/share/sleep suites stay unchanged.
 *
 * Run from the repository root (Node 24 / pnpm 10); builds + serves
 * locally, all APIs fulfilled in-test, no staging/prod access:
 *   CI=1 pnpm --dir apps/web test:e2e tests/library-repeat-end-policy.spec.ts --workers=1
 * Unit companion (offline):
 *   pnpm --dir apps/web test src/lib/library/libraryRepeatEndPolicy.acceptance.test.ts
 * Do not insert `--` after these pnpm scripts: this toolchain forwards it,
 * ending option parsing and selecting the entire suite. Ensure preview
 * port 4173 is free; never reuse another worktree's preview.
 *
 * RED NOW: `player-repeat` does not exist and the library audio has no `ended`
 * handler, so every case below fails at its control/advance assertion with a
 * NEXT action. No import/compile failure counts as RED: this file imports only
 * `@playwright/test`. Missing control is a SOFT assertion where transport is
 * also pinned: transport assertions still execute on baseline Off, proving
 * actual One/Queue failures rather than selector timeouts.
 * Every RED assertion message is a compact JSON receipt:
 * {"case","expected","actual","status","error_code","next_action"}.
 */

const ITEMS = [
	{ id: 'lib-coral', title: 'Coral Night', artist: 'Mira', streamUrl: '/test-audio/lib-coral' },
	{ id: 'lib-pine', title: 'Pine Morning', artist: 'North', streamUrl: '/test-audio/lib-pine' },
	{ id: 'lib-ember', title: 'Ember Line', artist: 'East', streamUrl: '/test-audio/lib-ember' }
];

const PLAUSIBLE_REPEAT_KEYS = [
	'nntune:library-repeat:v1',
	'nntune:repeat:v1',
	'nntune:player-repeat:v1',
	'nntune:library-repeat',
	'repeat'
];

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

/** Keep media synthetic and pending; stub clipboard/media per harness. */
async function openLibrary(
	page: Page,
	options: {
		items?: typeof ITEMS;
		seedCorruptRepeat?: boolean;
	} = {}
) {
	const { items = ITEMS, seedCorruptRepeat = false } = options;
	await page.route('**/api/library', (route) =>
		route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(items) })
	);
	await page.route('**/test-audio/**', () => {});
	await page.addInitScript(
		({ plausible, corrupt }: { plausible: string[]; corrupt: boolean }) => {
			const media = { play: 0, pause: 0, lastTime: -1, lastSrc: '' };
			(window as unknown as { __librepMedia: typeof media }).__librepMedia = media;
			HTMLMediaElement.prototype.play = function () {
				media.play += 1;
				media.lastTime = this.currentTime;
				media.lastSrc = this.getAttribute('src') ?? '';
				Object.defineProperty(this, 'paused', { value: false, configurable: true });
				Object.defineProperty(this, 'ended', { value: false, configurable: true });
				return Promise.resolve();
			};
			HTMLMediaElement.prototype.pause = function () {
				media.pause += 1;
				Object.defineProperty(this, 'paused', { value: true, configurable: true });
			};
			const clipboardCalls: string[] = [];
			(window as unknown as { __librepClipboard: string[] }).__librepClipboard = clipboardCalls;
			Object.defineProperty(navigator, 'clipboard', {
				value: {
					writeText: (text: string) => {
						clipboardCalls.push(text);
						return Promise.resolve();
					}
				},
				configurable: true
			});
			if (corrupt) {
				const garbage = [
					'{"mode":"bogus"',
					'"queue"x',
					'null',
					'42',
					'x'.repeat(70_000),
					'{"repeat":"forever"}'
				];
				for (const key of plausible) {
					for (const storage of [localStorage, sessionStorage]) {
						try {
							storage.setItem(key, garbage[key.length % garbage.length]!);
						} catch {
							/* storage may be unavailable; the fallback pin still applies */
						}
					}
				}
			}
		},
		{ plausible: PLAUSIBLE_REPEAT_KEYS, corrupt: seedCorruptRepeat }
	);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(items.length);
}

function row(page: Page, id: string) {
	return page.locator(`[data-testid="library-item"][data-id="${id}"]`);
}

/**
 * Click the row's select control (the button whose accessible name STARTS
 * with the track title). Star ("Star X") and copy ("Copy link X") buttons
 * share the title substring, so a plain name match would be ambiguous; the
 * `^Title` anchor keeps selection clicks on the select control.
 */
async function selectRow(page: Page, id: string, title: string) {
	const escaped = title.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
	await row(page, id)
		.getByRole('button', { name: new RegExp(`^${escaped}( — |$)`) })
		.click();
}

async function queueOrder(page: Page): Promise<(string | null)[]> {
	return page
		.getByTestId('queue-item')
		.evaluateAll((els) => els.map((el) => el.getAttribute('data-track-id') ?? el.getAttribute('data-id')));
}

async function mediaCalls(page: Page): Promise<{ play: number; pause: number; lastTime: number; lastSrc: string }> {
	return page.evaluate(
		() =>
			(window as unknown as { __librepMedia: { play: number; pause: number; lastTime: number; lastSrc: string } })
				.__librepMedia ?? { play: 0, pause: 0, lastTime: -1, lastSrc: '' }
	);
}

async function clipboardCalls(page: Page): Promise<string[]> {
	return page.evaluate(
		() => (window as unknown as { __librepClipboard: string[] }).__librepClipboard ?? []
	);
}

/** Best-effort mode selection: missing control is SOFT so transport still pins baseline Off. */
async function setMode(page: Page, caseId: string, value: 'off' | 'one' | 'queue') {
	const control = page.getByTestId('player-repeat');
	await expect
		.soft(
			control,
			note(
				caseId,
				`a repeat control accepting "${value}"`,
				'missing',
				'repeat_control_missing',
				'Builder adds <select data-testid="player-repeat"> with values off/one/queue; transport below must still fail on baseline Off.'
			)
		)
		.toHaveCount(1, { timeout: 1000 });
	if (await control.count()) await control.selectOption(value);
}

/** Simulate user playback intent on the current native audio element. */
async function playCurrent(page: Page) {
	await page.getByTestId('audio-player').evaluate((el) => {
		void (el as HTMLMediaElement).play();
		el.dispatchEvent(new Event('play'));
		el.dispatchEvent(new Event('playing'));
	});
}

async function fireEnded(page: Page) {
	await page.getByTestId('audio-player').evaluate((el) => {
		Object.defineProperty(el, 'paused', { value: true, configurable: true });
		Object.defineProperty(el, 'ended', { value: true, configurable: true });
		el.dispatchEvent(new Event('pause'));
		el.dispatchEvent(new Event('ended'));
	});
}

async function fireStaleEnded(page: Page) {
	await page.getByTestId('audio-player').evaluate((el) => {
		Object.defineProperty(el, 'paused', { value: true, configurable: true });
		Object.defineProperty(el, 'ended', { value: false, configurable: true });
		el.dispatchEvent(new Event('ended'));
	});
}

async function audioLoop(page: Page) {
	return page.getByTestId('audio-player').evaluate((el) => ({
		loop: (el as HTMLMediaElement).loop,
		attr: el.getAttribute('loop')
	}));
}

async function audioTime(page: Page) {
	return page.getByTestId('audio-player').evaluate((el) => (el as HTMLMediaElement).currentTime);
}

async function setAudioTime(page: Page, seconds: number) {
	await page
		.getByTestId('audio-player')
		.evaluate((el, value) => {
			(el as HTMLMediaElement).currentTime = value;
		}, seconds);
}

async function settle(page: Page) {
	// Two frames let synchronous handlers, promises and Svelte effects settle
	// before asserting the absence of an unwanted play request.
	await page.evaluate(
		() => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())))
	);
}

async function snapshotGuards(page: Page) {
	const favorites = await page
		.getByTestId('library-item')
		.getByRole('button', { name: /^(Star|Unstar) / })
		.evaluateAll((buttons) => buttons.map((button) => button.getAttribute('aria-pressed')));
	const selection = await page.getByTestId('selection-count').textContent();
	const findQuery = await page.locator('#library-find').inputValue().catch(() => null);
	const findScope = await page.locator('#library-find-scope').inputValue().catch(() => null);
	const order = await page.locator('#library-order').inputValue().catch(() => null);
	const view = await page.locator('#library-view').inputValue().catch(() => null);
	const queueFilter = await page.locator('#queue-filter-input').inputValue().catch(() => null);
	const shareStatus = await page.getByTestId('library-share-status').textContent();
	const shareFallback = await page.getByTestId('library-share-fallback').count();
	const storage = await page.evaluate(() => {
		const dump = (store: Storage) => {
			const out: Record<string, string | null> = {};
			for (let i = 0; i < store.length; i += 1) {
				const key = store.key(i);
				if (key !== null) out[key] = store.getItem(key);
			}
			return out;
		};
		return { local: dump(localStorage), session: dump(sessionStorage) };
	});
	return { favorites, selection, findQuery, findScope, order, view, queueFilter, shareStatus, shareFallback, storage };
}

async function expectGuardsUnchanged(
	page: Page,
	caseId: string,
	before: Awaited<ReturnType<typeof snapshotGuards>>,
	options: { allowRepeatStorageKeys?: boolean } = {}
) {
	const after = await snapshotGuards(page);
	expect(after.favorites, `${caseId}: favorites must not change on end-of-track`).toEqual(before.favorites);
	expect(after.selection, `${caseId}: subset selection must not change on end-of-track`).toBe(before.selection);
	expect(after.findQuery, `${caseId}: find query must not change on end-of-track`).toBe(before.findQuery);
	expect(after.findScope, `${caseId}: find scope must not change on end-of-track`).toBe(before.findScope);
	expect(after.order, `${caseId}: library order must not change on end-of-track`).toBe(before.order);
	expect(after.view, `${caseId}: library view must not change on end-of-track`).toBe(before.view);
	expect(after.queueFilter, `${caseId}: queue filter must not change on end-of-track`).toBe(before.queueFilter);
	expect(after.shareStatus ?? '', `${caseId}: share status must stay clear on end-of-track`).toBe('');
	expect(after.shareFallback, `${caseId}: share fallback must stay hidden on end-of-track`).toBe(0);
	expect(await clipboardCalls(page), `${caseId}: clipboard must stay untouched on end-of-track`).toEqual([]);
	if (!options.allowRepeatStorageKeys) {
		expect(after.storage, `${caseId}: storage must stay untouched on end-of-track`).toEqual(before.storage);
	}
}

async function expectAdvanceAnnouncement(page: Page, caseId: string, title: string) {
	const announcement = page.getByTestId('queue-announcement');
	await expect(
		announcement,
		note(
			caseId,
			`one concise polite announcement naming "${title}"`,
			'missing or silent',
			'advance_announcement_missing',
			'Builder sets queue-announcement to one concise (<140 chars) polite message naming the new track on every automatic advance.'
		)
	).not.toHaveText('', { timeout: 3000 });
	const text = (await announcement.textContent()) ?? '';
	expect(text.length, `${caseId}: announcement must stay concise`).toBeLessThanOrEqual(140);
	expect(text, `${caseId}: announcement must name the new track`).toContain(title);
	await expect(announcement, `${caseId}: announcement must stay polite`).toHaveAttribute('role', 'status');
}

async function expectSilentAnnouncement(page: Page, caseId: string) {
	await settle(page);
	await expect(
		page.getByTestId('queue-announcement'),
		`${caseId}: silent transitions must not chatter the announcement region`
	).toHaveText('');
}

test('[LIBREP-01] repeat control is a labeled native select defaulting to Off with a live status and focus kept', async ({
	page
}) => {
	await openLibrary(page);
	const control = page.getByTestId('player-repeat');
	await expect(
		control,
		note(
			'LIBREP-01',
			'a labeled native repeat select defaulting to Off',
			'missing',
			'repeat_control_missing',
			'Builder adds <select data-testid="player-repeat"> with accessible name Repeat, values off/one/queue, default Off.'
		)
	).toBeVisible();
	await expect(control).toHaveJSProperty('tagName', 'SELECT');
	await expect(control).toHaveAccessibleName(/repeat/i);
	await expect(control).toHaveValue('off');
	const status = page.getByTestId('repeat-status');
	await expect(status).toHaveAttribute('role', 'status');
	await expect(status).toContainText(/off/i);
	for (const value of ['one', 'queue', 'off'] as const) {
		await control.focus();
		await control.selectOption(value);
		await expect(control).toBeFocused();
		await expect(status).toContainText(new RegExp(value, 'i'));
	}
	// Persistence: the mode survives reload (unlike the demo, which resets).
	await control.selectOption('queue');
	await page.reload();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(page.getByTestId('player-repeat')).toHaveValue('queue');
	await expect(page.getByTestId('repeat-status')).toContainText(/queue/i);
});

test('[LIBREP-02] Off advances mid-queue from zero with one announcement and no guard mutation', async ({
	page
}) => {
	await openLibrary(page);
	await setMode(page, 'LIBREP-02', 'off');
	await selectRow(page, 'lib-coral', 'Coral Night');
	await selectRow(page, 'lib-pine', 'Pine Morning');
	await selectRow(page, 'lib-ember', 'Ember Line');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(page.getByTestId('audio-player')).toHaveCount(1);
	await playCurrent(page);
	await setAudioTime(page, 23);
	const guards = await snapshotGuards(page);
	const before = await mediaCalls(page);
	await fireEnded(page);
	await expect(
		page.getByTestId('now-playing'),
		note(
			'LIBREP-02',
			'Off advances mid-queue to Pine Morning',
			'stayed on Coral Night',
			'off_advance_missing',
			'Builder routes library ended through naturalEndPolicy so Off advances to the next queued track.'
		)
	).toContainText('Pine Morning', { timeout: 3000 });
	await expect(page.getByTestId('library-now-playing')).toContainText('Pine Morning');
	await expect
		.poll(
			() => mediaCalls(page).then((calls) => calls.play),
			note(
				'LIBREP-02',
				'Off advance issues one play request for the next track',
				'no play request',
				'off_advance_no_play',
				'Builder plays the advanced-to track from zero via the existing audio element.'
			)
		)
		.toBe(before.play + 1);
	const last = await mediaCalls(page);
	expect(last.lastTime, 'LIBREP-02: the next track must start from zero').toBe(0);
	expect(last.lastSrc, 'LIBREP-02: the next track src must be the pine stream').toContain('lib-pine');
	expect(await audioTime(page), 'LIBREP-02: the new audio must start at zero').toBe(0);
	expect(await queueOrder(page)).toEqual(['lib-coral', 'lib-pine', 'lib-ember']);
	await expect(page.getByTestId('radio-badge')).toHaveCount(0);
	await expectAdvanceAnnouncement(page, 'LIBREP-02', 'Pine Morning');
	await expectGuardsUnchanged(page, 'LIBREP-02', guards, { allowRepeatStorageKeys: true });
	const loop = await audioLoop(page);
	expect(loop.loop, 'LIBREP-02: native loop must stay false').toBe(false);
	expect(loop.attr, 'LIBREP-02: no loop attribute may be rendered').toBeNull();
});

test('[LIBREP-03] Off stops truthfully at the tail and stale ends stay inert', async ({ page }) => {
	await openLibrary(page);
	await setMode(page, 'LIBREP-03', 'off');
	await selectRow(page, 'lib-coral', 'Coral Night');
	await selectRow(page, 'lib-pine', 'Pine Morning');
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Pine Morning');
	await playCurrent(page);
	await setAudioTime(page, 41);
	const guards = await snapshotGuards(page);
	const before = await mediaCalls(page);
	await fireEnded(page);
	await settle(page);
	expect(
		await mediaCalls(page).then((calls) => calls.play),
		note(
			'LIBREP-03',
			'Off at the tail stops without a play request',
			'unexpected play or advance',
			'off_tail_stop_missing',
			'Builder returns naturalEndPolicy stop at the queue end under Off: no play, no fabricated track.'
		)
	).toBe(before.play);
	await expect(page.getByTestId('now-playing')).toContainText('Pine Morning');
	await expect(page.getByTestId('library-now-playing')).toContainText('Pine Morning');
	expect(await queueOrder(page)).toEqual(['lib-coral', 'lib-pine']);
	await expect(page.getByTestId('radio-badge')).toHaveCount(0);
	await expectSilentAnnouncement(page, 'LIBREP-03');
	await fireStaleEnded(page);
	await settle(page);
	expect(await mediaCalls(page).then((calls) => calls.play)).toBe(before.play);
	await expect(page.getByTestId('now-playing')).toContainText('Pine Morning');
	await expectSilentAnnouncement(page, 'LIBREP-03-stale');
	await expectGuardsUnchanged(page, 'LIBREP-03', guards, { allowRepeatStorageKeys: true });
});

test('[LIBREP-04] One replays the current track from zero without moving the selection', async ({
	page
}) => {
	await openLibrary(page);
	await setMode(page, 'LIBREP-04', 'one');
	await selectRow(page, 'lib-coral', 'Coral Night');
	await selectRow(page, 'lib-pine', 'Pine Morning');
	await selectRow(page, 'lib-ember', 'Ember Line');
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Pine Morning');
	await playCurrent(page);
	await setAudioTime(page, 31);
	const guards = await snapshotGuards(page);
	const before = await mediaCalls(page);
	await fireEnded(page);
	await expect(
		page.getByTestId('now-playing'),
		note(
			'LIBREP-04',
			'One replays Pine Morning without advancing',
			'advanced or stopped',
			'one_replay_missing',
			'Builder replays the current track from zero under One via naturalEndPolicy.'
		)
	).toContainText('Pine Morning', { timeout: 3000 });
	await expect
		.poll(
			() => mediaCalls(page).then((calls) => calls.play),
			note(
				'LIBREP-04',
				'One replay issues one play request from zero',
				'no play request',
				'one_replay_no_play',
				'Builder resets currentTime to zero and plays the current track under One.'
			)
		)
		.toBe(before.play + 1);
	const last = await mediaCalls(page);
	expect(last.lastTime, 'LIBREP-04: replay must start from zero').toBe(0);
	expect(await audioTime(page), 'LIBREP-04: replayed audio must reset to zero').toBe(0);
	expect(await queueOrder(page)).toEqual(['lib-coral', 'lib-pine', 'lib-ember']);
	await expect(page.getByTestId('radio-badge')).toHaveCount(0);
	await expectAdvanceAnnouncement(page, 'LIBREP-04', 'Pine Morning');
	await expectGuardsUnchanged(page, 'LIBREP-04', guards, { allowRepeatStorageKeys: true });
	const loop = await audioLoop(page);
	expect(loop.loop, 'LIBREP-04: native loop must stay false').toBe(false);
	expect(loop.attr, 'LIBREP-04: no loop attribute may be rendered').toBeNull();
});

test('[LIBREP-05] Queue follows the edited order then wraps to the first track', async ({ page }) => {
	await openLibrary(page);
	await setMode(page, 'LIBREP-05', 'queue');
	// Current starts on the first selection (pine); moves then reshape the
	// arrangement around it, so the current track is mid-queue in edited order.
	await selectRow(page, 'lib-pine', 'Pine Morning');
	await selectRow(page, 'lib-coral', 'Coral Night');
	await selectRow(page, 'lib-ember', 'Ember Line');
	// Arrange ember first via the queue move controls (edited order must win).
	const moveUp = page
		.locator('[data-testid="queue-item"][data-track-id="lib-ember"]')
		.getByRole('button', { name: /move .* up/i });
	await moveUp.click();
	await moveUp.click();
	expect(await queueOrder(page)).toEqual(['lib-ember', 'lib-pine', 'lib-coral']);
	await expect(page.getByTestId('now-playing')).toContainText('Pine Morning');
	// Re-selecting an already-queued row clears the stale move announcement
	// without touching order or selection, so the advance pin starts silent.
	await selectRow(page, 'lib-coral', 'Coral Night');
	expect(await queueOrder(page)).toEqual(['lib-ember', 'lib-pine', 'lib-coral']);
	await expect(page.getByTestId('now-playing')).toContainText('Pine Morning');
	await expect(page.getByTestId('queue-announcement')).toHaveText('');
	await playCurrent(page);
	const guards = await snapshotGuards(page);
	const before = await mediaCalls(page);
	await fireEnded(page);
	await expect(
		page.getByTestId('now-playing'),
		note(
			'LIBREP-05',
			'Queue follows the edited order to Coral Night',
			'did not follow the edited order',
			'queue_order_missing',
			'Builder advances through the arranged queue under Queue via naturalEndPolicy.'
		)
	).toContainText('Coral Night', { timeout: 3000 });
	await expectAdvanceAnnouncement(page, 'LIBREP-05-advance', 'Coral Night');
	// Tail wraps to the arranged first track (ember), never to radio.
	await fireEnded(page);
	await expect(
		page.getByTestId('now-playing'),
		note(
			'LIBREP-05-wrap',
			'Queue wraps the tail to Ember Line',
			'stopped or left the queue',
			'queue_wrap_missing',
			'Builder wraps the queue tail to the first arranged track under Queue, without radio.'
		)
	).toContainText('Ember Line', { timeout: 3000 });
	expect(await mediaCalls(page).then((calls) => calls.play)).toBeGreaterThan(before.play);
	expect(await queueOrder(page)).toEqual(['lib-ember', 'lib-pine', 'lib-coral']);
	await expect(page.getByTestId('radio-badge')).toHaveCount(0);
	await expectAdvanceAnnouncement(page, 'LIBREP-05-wrap', 'Ember Line');
	await expectGuardsUnchanged(page, 'LIBREP-05', guards, { allowRepeatStorageKeys: true });
});

test('[LIBREP-06] empty queues never render audio or autoplay', async ({ page }) => {
	await openLibrary(page);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
	await expect(
		page.getByTestId('audio-player'),
		note(
			'LIBREP-06',
			'no audio element on an empty queue',
			'audio rendered without a track',
			'empty_autoplay',
			'Builder renders audio only for a current track with a usable streamUrl; empty queues never autoplay.'
		)
	).toHaveCount(0);
	expect(await mediaCalls(page).then((calls) => calls.play)).toBe(0);
	await expect(page.getByTestId('radio-badge')).toHaveCount(0);
	await expectSilentAnnouncement(page, 'LIBREP-06');
});

for (const value of ['one', 'queue'] as const) {
	test(`[LIBREP-07-${value}] a singleton replays instead of fabricating a next track`, async ({
		page
	}) => {
		await openLibrary(page);
		await setMode(page, `LIBREP-07-${value}`, value);
		await selectRow(page, 'lib-pine', 'Pine Morning');
		await expect(page.getByTestId('queue-item')).toHaveCount(1);
		await playCurrent(page);
		await setAudioTime(page, 17);
		const before = await mediaCalls(page);
		await fireEnded(page);
		await expect(
			page.getByTestId('now-playing'),
			note(
				`LIBREP-07-${value}`,
				`singleton replays Pine Morning under ${value}`,
				'stopped or fabricated a track',
				'singleton_replay_missing',
				`Builder replays the only track from zero under ${value} without fabricating a next track.`
			)
		).toContainText('Pine Morning', { timeout: 3000 });
		await expect
			.poll(
				() => mediaCalls(page).then((calls) => calls.play),
				note(
					`LIBREP-07-${value}`,
					`singleton replay issues one play request under ${value}`,
					'no play request',
					'singleton_replay_no_play',
					`Builder replays the only track from zero under ${value}.`
				)
			)
			.toBe(before.play + 1);
		expect(await queueOrder(page)).toEqual(['lib-pine']);
		await expect(page.getByTestId('radio-badge')).toHaveCount(0);
		await expectAdvanceAnnouncement(page, `LIBREP-07-${value}`, 'Pine Morning');
	});
}

test('[LIBREP-07-off] a singleton stops truthfully under Off without fabricating a track', async ({
	page
}) => {
	await openLibrary(page);
	await setMode(page, 'LIBREP-07-off', 'off');
	await selectRow(page, 'lib-pine', 'Pine Morning');
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	await playCurrent(page);
	const before = await mediaCalls(page);
	await fireEnded(page);
	await settle(page);
	expect(
		await mediaCalls(page).then((calls) => calls.play),
		note(
			'LIBREP-07-off',
			'singleton stops under Off with no play request',
			'advanced or replayed',
			'singleton_stop_missing',
			'Builder stops a singleton queue under Off: no play, no fabricated next track.'
		)
	).toBe(before.play);
	await expect(page.getByTestId('now-playing')).toContainText('Pine Morning');
	expect(await queueOrder(page)).toEqual(['lib-pine']);
	await expectSilentAnnouncement(page, 'LIBREP-07-off');
});

test('[LIBREP-08] manual pause and stale ended stay inert under every mode', async ({ page }) => {
	for (const value of ['off', 'one', 'queue'] as const) {
		await openLibrary(page);
		await setMode(page, `LIBREP-08-${value}`, value);
		await selectRow(page, 'lib-coral', 'Coral Night');
		await selectRow(page, 'lib-pine', 'Pine Morning');
		await playCurrent(page);
		// Manual pause first: a bare ended without a valid completion must not advance.
		await page.getByTestId('audio-player').evaluate((el) => {
			Object.defineProperty(el, 'paused', { value: true, configurable: true });
			Object.defineProperty(el, 'ended', { value: false, configurable: true });
			el.dispatchEvent(new Event('pause'));
		});
		const before = await mediaCalls(page);
		await page.getByTestId('audio-player').evaluate((el) => el.dispatchEvent(new Event('ended')));
		await settle(page);
		expect(
			await mediaCalls(page).then((calls) => calls.play),
			note(
				`LIBREP-08-${value}`,
				`paused ended stays inert under ${value}`,
				'advanced while paused',
				'paused_end_advances',
				'Builder ignores ended events without a valid playing completion under every mode.'
			)
		).toBe(before.play);
		await expect(page.getByTestId('now-playing')).toContainText('Coral Night');
		await expectSilentAnnouncement(page, `LIBREP-08-${value}`);
	}
});

test('[LIBREP-09] corrupt persisted modes fall back to Off', async ({ page }) => {
	await openLibrary(page, { seedCorruptRepeat: true });
	const control = page.getByTestId('player-repeat');
	await expect(
		control,
		note(
			'LIBREP-09',
			'a repeat control defaulting to Off despite corrupt storage',
			'missing',
			'repeat_control_missing',
			'Builder adds the persisted repeat select; corrupt stored values fall back to Off.'
		)
	).toBeVisible({ timeout: 5000 });
	await expect(control).toHaveValue('off');
	await expect(page.getByTestId('repeat-status')).toContainText(/off/i);
	// Discover the real storage key by setting Queue, corrupt it, and reload.
	await control.selectOption('queue');
	const repeatKey = await page.evaluate(() => {
		const scan = (store: Storage): string | null => {
			for (let i = 0; i < store.length; i += 1) {
				const key = store.key(i);
				if (!key) continue;
				const value = store.getItem(key) ?? '';
				if (value.toLowerCase().includes('queue') && key.toLowerCase().includes('repeat')) return key;
			}
			return null;
		};
		return scan(localStorage) ?? scan(sessionStorage);
	});
	if (repeatKey) {
		await page.evaluate((key) => {
			for (const store of [localStorage, sessionStorage]) {
				try {
					if (store.getItem(key) !== null) store.setItem(key, '{"mode":"bogus"');
				} catch {
					/* ignore unavailable storage */
				}
			}
		}, repeatKey);
		await page.reload();
		await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
		await expect(page.getByTestId('player-repeat')).toHaveValue('off');
		await expect(page.getByTestId('repeat-status')).toContainText(/off/i);
	}
});

test('[LIBREP-10] auth teardown clears pending end handling without advancing', async ({ page }) => {
	const payload: { current: unknown } = { current: ITEMS };
	await page.route('**/api/library', (route) =>
		route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(payload.current) })
	);
	await page.route('**/test-audio/**', () => {});
	await page.addInitScript(() => {
		const media = { play: 0 };
		(window as unknown as { __librepMedia: typeof media }).__librepMedia = media;
		HTMLMediaElement.prototype.play = function () {
			media.play += 1;
			return Promise.resolve();
		};
		HTMLMediaElement.prototype.pause = function () {};
	});
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await setMode(page, 'LIBREP-10', 'queue');
	await selectRow(page, 'lib-coral', 'Coral Night');
	await selectRow(page, 'lib-pine', 'Pine Morning');
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	await playCurrent(page);
	const audio = await page.getByTestId('audio-player').elementHandle();
	const before = await mediaCalls(page).then((calls) => calls.play);
	// Cross-tab auth transition: the queue clears before the next session retry.
	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate((key) => {
		window.localStorage.setItem(key, 'test-auth-transition');
		window.localStorage.removeItem(key);
	}, 'nntune.library.auth-transition.v1');
	await otherTab.close();
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	expect(await audio!.evaluate((el) => el.isConnected)).toBe(false);
	// A detached ended from the previous session must not resurrect playback.
	await audio!.evaluate((el) => {
		Object.defineProperty(el, 'ended', { value: true, configurable: true });
		el.dispatchEvent(new Event('ended'));
	});
	await settle(page);
	expect(
		await mediaCalls(page).then((calls) => calls.play),
		note(
			'LIBREP-10',
			'detached ended after auth teardown starts nothing',
			'playback resurrected across the auth boundary',
			'auth_teardown_advances',
			'Builder clears pending end handling at the auth boundary; detached ends never advance into the next session.'
		)
	).toBe(before);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	// The error branch unmounts the queue panel; if it stays mounted its
	// announcement must be clear — either way no stale text may linger.
	if (await page.getByTestId('queue-announcement').count()) {
		await expect(page.getByTestId('queue-announcement')).toHaveText('');
	}
	payload.current = [{ id: 'lib-bravo', title: 'Bravo Song', streamUrl: '/test-audio/lib-bravo' }];
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expect(page.getByTestId('library-list')).toContainText('Bravo Song');
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	expect(await mediaCalls(page).then((calls) => calls.play)).toBe(before);
});

test('[LIBREP-11] a stop stays terminal: no advance after it under any mode', async ({ page }) => {
	// The library ships no sleep UI (sleep-timer changes are out of scope for
	// LIBREPEAT); the browser pin is stop-terminal behavior — the same "no
	// advance after a stop" a fired timer requires — while the pure vectors
	// pin sleepComplete→stop for every mode.
	for (const value of ['off', 'one', 'queue'] as const) {
		await openLibrary(page);
		await setMode(page, `LIBREP-11-${value}`, value);
		await selectRow(page, 'lib-coral', 'Coral Night');
		await selectRow(page, 'lib-pine', 'Pine Morning');
		await page.getByTestId('player-next').click();
		await expect(page.getByTestId('now-playing')).toContainText('Pine Morning');
		await playCurrent(page);
		if (value === 'off') {
			const before = await mediaCalls(page);
			await fireEnded(page);
			await settle(page);
			expect(
				await mediaCalls(page).then((calls) => calls.play),
				note(
					'LIBREP-11-off',
					'Off tail stop starts nothing further',
					'advanced after the stop',
					'stop_not_terminal',
					'Builder leaves Off tail stops terminal: later ended events never advance.'
				)
			).toBe(before.play);
			await fireEnded(page);
			await settle(page);
			expect(await mediaCalls(page).then((calls) => calls.play)).toBe(before.play);
			await expect(page.getByTestId('now-playing')).toContainText('Pine Morning');
			await expectSilentAnnouncement(page, 'LIBREP-11-off');
		} else {
			// Clearing mid-mode must not let a detached end resurrect playback.
			const audio = await page.getByTestId('audio-player').elementHandle();
			await page.getByRole('button', { name: 'Clear queue', exact: true }).click();
			await expect(page.getByTestId('queue-item')).toHaveCount(0);
			const before = await mediaCalls(page).then((calls) => calls.play);
			await audio!.evaluate((el) => el.dispatchEvent(new Event('ended')));
			await settle(page);
			expect(
				await mediaCalls(page).then((calls) => calls.play),
				note(
					`LIBREP-11-${value}`,
					`detached ended after clear starts nothing under ${value}`,
					'playback resurrected after clear',
					'clear_resurrects',
					'Builder ignores detached ends after the queue clears under every mode.'
				)
			).toBe(before);
			await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
		}
	}
});

test('[LIBREP-12] explicit Next stays clamped at the tail under every mode', async ({ page }) => {
	for (const value of ['off', 'one', 'queue'] as const) {
		await openLibrary(page);
		await setMode(page, `LIBREP-12-${value}`, value);
		await selectRow(page, 'lib-coral', 'Coral Night');
		await selectRow(page, 'lib-pine', 'Pine Morning');
		await page.getByTestId('player-next').click();
		await expect(page.getByTestId('now-playing')).toContainText('Pine Morning');
		const before = await mediaCalls(page);
		await page.getByTestId('player-next').click();
		await settle(page);
		expect(
			await mediaCalls(page).then((calls) => calls.play),
			note(
				`LIBREP-12-${value}`,
				`explicit Next at the tail stays clamped under ${value}`,
				'wrapped or replayed',
				'explicit_next_wraps',
				'Builder keeps explicit Next clamped: repeat never traps a deliberate Next.'
			)
		).toBe(before.play);
		await expect(page.getByTestId('now-playing')).toContainText('Pine Morning');
		expect(await queueOrder(page)).toEqual(['lib-coral', 'lib-pine']);
	}
});
