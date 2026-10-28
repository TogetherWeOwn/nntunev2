import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Acceptance-owned regressions for destructive queue consent. Keep the original
 * queue-clear-confirm.spec.ts bank intact. These cases exercise the real library
 * route; only its external library/media responses are fixtures.
 *
 * Breaks caught: consent reused after implicit reconciliation, focus stranded on
 * a disabled/disconnected control, offscreen recovery, and undisclosed scope
 * changes. Explicit Next may update an open prompt; implicit replacement must
 * not silently retarget consent. DOM .click() models virtual activation without
 * manufacturing a focus move. Viewport assertions precede any scrolling click.
 *
 * Run from apps/web:
 *   pnpm exec playwright test tests/queue-clear-confirm-regressions.spec.ts --workers=1
 *   pnpm check
 * Product fixes and full feature GREEN remain the paired Builder's work.
 */

type Item = { id: string; title: string; artist?: string; streamUrl?: string };
const ITEMS: Item[] = [
	{ id: 'consent-first', title: 'First Song' },
	{ id: 'consent-second', title: 'Second Song' },
	{ id: 'consent-third', title: 'Third Song' }
];
const IRREVERSIBLE = /cannot be undone|irreversible|permanently|no undo/i;
const yes = (page: Page) => page.getByRole('button', { name: /^(confirm|yes\b|clear\s+\d+)/i });
const cancel = (page: Page) => page.getByRole('button', { name: /^cancel\b/i });
const clear = (page: Page) => page.getByRole('button', { name: 'Clear queue', exact: true });
const trim = (page: Page) => page.getByRole('button', { name: 'Clear upcoming', exact: true });
const filter = (page: Page) => page.getByLabel('Filter queue', { exact: true });
const current = (page: Page) => page.locator('[data-testid="queue-item"][aria-current="true"]');
const item = (page: Page, id: string) => page.locator(`[data-testid="library-item"][data-id="${id}"]`);
const queueItem = (page: Page, id: string) =>
	page.locator(`[data-testid="queue-item"][data-track-id="${id}"]`);

async function order(page: Page): Promise<string[]> {
	return page.getByTestId('queue-item').evaluateAll((nodes) =>
		nodes.map((node) => node.getAttribute('data-track-id')!)
	);
}

async function flush(page: Page): Promise<void> {
	await page.evaluate(() => new Promise<void>((resolve) =>
		requestAnimationFrame(() => requestAnimationFrame(() => resolve()))
	));
}

async function library(page: Page, items: Item[] = ITEMS): Promise<void> {
	await page.route('**/api/library', (route) => route.fulfill({ json: items }));
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(items.length);
}

async function enqueue(page: Page, items: Item[] = ITEMS): Promise<void> {
	for (const track of items) await item(page, track.id).click();
	await expect.poll(() => order(page)).toEqual(items.map((track) => track.id));
}

async function virtualConfirm(page: Page): Promise<void> {
	await yes(page).evaluate((element: HTMLButtonElement) => element.click());
}

async function expectLiveRecovery(target: Locator): Promise<void> {
	await expect(target).toBeFocused();
	await expect(target).toBeEnabled();
	await expect(target).toBeVisible();
	await expect(target).toBeInViewport();
	expect(await target.evaluate((element) => element.isConnected)).toBe(true);
}

/** Resolve refresh explicitly; no sleeps or overlapping requests at teardown. */
async function gatedLibrary(page: Page, initial: Item[], refreshed: Item[], url = '/library', leaveShareUnconsumed = false) {
	let requests = 0;
	let release!: () => void;
	let releaseInitial!: () => void;
	const gate = new Promise<void>((resolve) => { release = resolve; });
	const initialGate = new Promise<void>((resolve) => { releaseInitial = resolve; });
	await page.route('**/api/library', async (route) => {
		requests += 1;
		const first = requests === 1;
		await (first ? initialGate : gate);
		await route.fulfill({ json: first ? initial : refreshed });
	});
	try {
		await page.goto(url);
		if (leaveShareUnconsumed) {
			await expect.poll(() => requests).toBe(1);
			// Leave the share URL via a real link BEFORE the first library result.
			// Its history entry has never been resolved/consumed against any items.
			await sameRouteLink(page, '/library');
		}
	} finally {
		releaseInitial();
	}
	await expect(page.getByTestId('library-item')).toHaveCount(initial.length);
	return {
		release,
		async start() {
			await page.getByRole('button', { name: 'Refresh', exact: true }).click();
			await expect.poll(() => requests).toBe(2);
		},
		async finish() {
			release();
			await expect(page.getByRole('button', { name: 'Refresh', exact: true })).toBeEnabled();
			await expect(page.getByTestId('library-item')).toHaveCount(refreshed.length);
			await flush(page);
		}
	};
}

async function sameRouteLink(page: Page, href: string): Promise<void> {
	await page.evaluate((href) => {
		const anchor = document.createElement('a');
		anchor.id = 'consent-navigation';
		anchor.href = href;
		anchor.textContent = 'Open library link';
		document.body.append(anchor);
	}, href);
	await page.locator('#consent-navigation').click();
	await expect(page).toHaveURL((url) => `${url.pathname}${url.search}` === href);
	await page.locator('#consent-navigation').evaluate((element) => element.remove());
}

test('QCR-01 batched prune and unconsumed Back share cannot reuse all-clear consent', async ({ page }) => {
	await nativeAudio(page);
	const a = { id: 'consent-a', title: 'Original A', streamUrl: '/test-audio/consent-a' };
	const b = { id: 'consent-b', title: 'Replacement B', streamUrl: '/test-audio/consent-b' };
	const load = await gatedLibrary(page, [a], [b], '/library?track=consent-b&t=0', true);
	try {
		await enqueue(page, [a]);
		await page.evaluate(() => { document.documentElement.dataset.consentDocument = 'original'; });
		await clear(page).click();
		await expect(yes(page)).toBeVisible();
		const staleYes = await yes(page).elementHandle();
		await load.start();
		await page.goBack();
		await expect(page).toHaveURL(/\/library\?track=consent-b&t=0$/);
		await expect(page.locator('html')).toHaveAttribute('data-consent-document', 'original');
		expect(await order(page)).toEqual(['consent-a']);
		await load.finish();
		await expect.poll(() => order(page)).toEqual(['consent-b']);
		await expect(current(page)).toHaveAttribute('data-track-id', 'consent-b');
		await expect.configure({ soft: true }).poll(async () => await yes(page).count() > 0 && await yes(page).isEnabled(),
			{ message: 'old consent must be dismissed or incapable of clearing the replacement' }).toBe(false);
		await staleYes!.evaluate((element: HTMLButtonElement) => element.click());
		await flush(page);
		expect(await order(page), 'a captured stale Yes must not clear replacement B').toEqual(['consent-b']);
		await clear(page).click();
		await expect(yes(page)).toBeEnabled();
		await yes(page).click();
		await expect(page.getByTestId('queue-item')).toHaveCount(0);
		await expectLiveRecovery(page.getByTestId('queue-empty-focus'));
	} finally {
		load.release();
	}
});

test('QCR-02 implicit same-count refresh cannot retarget pending upcoming consent', async ({ page }) => {
	const tracks = [
		{ id: 'prev-one', title: 'Previous One' },
		{ id: 'prev-two', title: 'Previous Two' },
		{ id: 'removed-current', title: 'Original Current' },
		{ id: 'next-one', title: 'Upcoming One' },
		{ id: 'next-two', title: 'Upcoming Two' }
	];
	const load = await gatedLibrary(page, tracks, [tracks[0]!, tracks[1]!, tracks[3]!]);
	try {
		await enqueue(page, tracks);
		await queueItem(page, 'removed-current').getByRole('button', { name: 'Select Original Current', exact: true }).click();
		await expect(current(page)).toHaveAttribute('data-track-id', 'removed-current');
		await load.start();
		await trim(page).click();
		await expect(yes(page)).toBeVisible();
		const staleYes = await yes(page).elementHandle();
		await expect(page.getByTestId('queue-position')).toContainText('2 upcoming');
		await load.finish();
		await expect(current(page)).toHaveAttribute('data-track-id', 'prev-one');
		await expect(page.getByTestId('queue-position')).toContainText('2 upcoming');
		await staleYes!.evaluate((element: HTMLButtonElement) => element.click());
		await flush(page);
		expect(await order(page), 'same count is not consent to remove different implicit successors')
			.toEqual(['prev-one', 'prev-two', 'next-one']);
		// A fresh deliberate request must continue to work after invalidation.
		await trim(page).click();
		await expect(yes(page)).toBeEnabled();
		await yes(page).click();
		await expect.poll(() => order(page)).toEqual(['prev-one']);
	} finally {
		load.release();
	}
});

// Adapted from sound-preference/fixtures.ts: native PCM, truthful Range replies.
// Only currentTime is sought; ended/paused/duration/play retain native semantics.
async function nativeAudio(page: Page): Promise<void> {
	const size = 4 * 8_000 * 2;
	const wav = Buffer.alloc(44 + size);
	wav.write('RIFF', 0); wav.writeUInt32LE(36 + size, 4); wav.write('WAVEfmt ', 8);
	wav.writeUInt32LE(16, 16); wav.writeUInt16LE(1, 20); wav.writeUInt16LE(1, 22);
	wav.writeUInt32LE(8_000, 24); wav.writeUInt32LE(16_000, 28);
	wav.writeUInt16LE(2, 32); wav.writeUInt16LE(16, 34);
	wav.write('data', 36); wav.writeUInt32LE(size, 40);
	await page.route('**/test-audio/consent-*', (route) => {
		const range = /^bytes=(\d+)-(\d*)$/.exec(route.request().headers()['range'] ?? '');
		const start = range ? Number(range[1]) : 0;
		const end = range?.[2] ? Math.min(Number(range[2]), wav.length - 1) : wav.length - 1;
		if (start >= wav.length || start > end) return route.fulfill({
			status: 416, headers: { 'content-range': `bytes */${wav.length}` }, body: ''
		});
		return route.fulfill({ status: range ? 206 : 200, contentType: 'audio/wav',
			headers: { 'accept-ranges': 'bytes', 'content-length': String(end - start + 1),
				...(range ? { 'content-range': `bytes ${start}-${end}/${wav.length}` } : {}) },
			body: wav.subarray(start, end + 1)
		});
	});
}

for (const newerFilter of [false, true]) {
	test(`QCR-03 trusted natural end dismisses upcoming with ${newerFilter ? 'newer filter focus preserved' : 'live visible recovery from Cancel'}`, async ({ page }) => {
		await nativeAudio(page);
		const tracks = ITEMS.slice(0, 2).map((track) => ({ ...track, streamUrl: `/test-audio/${track.id}` }));
		await library(page, tracks);
		await enqueue(page, tracks);
		await page.getByTestId('player-repeat').selectOption('off');
		await expect(page.getByTestId('repeat-status')).toContainText(/Repeat:\s*Off/);
		const audio = page.getByTestId('audio-player');
		await expect.poll(() => audio.evaluate((element: HTMLAudioElement) => element.readyState)).toBeGreaterThanOrEqual(2);
		await expect.poll(() => audio.evaluate((element: HTMLAudioElement) => element.duration)).toBe(4);
		await trim(page).click();
		await expect(cancel(page)).toBeFocused();
		if (newerFilter) {
			await filter(page).focus();
			await expect(filter(page)).toBeFocused();
		}
		await audio.evaluate(async (element: HTMLAudioElement) => {
			element.addEventListener('ended', (event) => {
				document.documentElement.dataset.consentTrustedEnd = String(event.isTrusted);
			}, { capture: true, once: true });
			element.currentTime = element.duration - 0.15;
			await element.play();
		});
		await expect(page.locator('html'), 'must observe browser-generated ended, never a dispatched surrogate')
			.toHaveAttribute('data-consent-trusted-end', 'true');
		await expect(current(page)).toHaveAttribute('data-track-id', 'consent-second');
		await expect(trim(page)).toBeDisabled();
		await expect(yes(page)).toHaveCount(0);
		await expect(cancel(page)).toHaveCount(0);
		await flush(page);
		if (newerFilter) await expect(filter(page)).toBeFocused();
		else {
			await expect.poll(() => page.evaluate(() => document.activeElement?.tagName),
				{ message: 'automatic dismissal must recover focus, never leave it on body' }).not.toBe('BODY');
			const active = page.locator(':focus');
			await expectLiveRecovery(active);
			expect(await active.evaluate((element) => element.matches(
				'[data-testid="queue-item"][aria-current="true"], [data-testid="queue-reveal-current"], input[aria-label="Filter queue"]'
			) || (element instanceof HTMLButtonElement && element.textContent?.trim() === 'Clear queue')),
			'automatic dismissal needs an appropriate surviving queue target').toBe(true);
		}
		expect(await order(page)).toEqual(['consent-first', 'consent-second']);
	});
}

for (const control of ['reveal', 'next'] as const) {
	test(`QCR-04 ${control} newly disabled by a virtual all-clear recovers to the empty target`, async ({ page }) => {
		await library(page);
		await enqueue(page);
		await clear(page).click();
		await expect(yes(page)).toBeVisible();
		const focused = page.getByTestId(control === 'reveal' ? 'queue-reveal-current' : 'player-next');
		await expect(focused).toBeEnabled();
		await focused.focus();
		await expect(focused).toBeFocused();
		await virtualConfirm(page);
		await expect(page.getByTestId('queue-item')).toHaveCount(0);
		await expect(focused).toBeDisabled();
		await expectLiveRecovery(page.getByTestId('queue-empty-focus'));
	});
}

for (const hidden of [false, true]) {
	test(`QCR-04 virtual trim recovers newly disabled shuffle focus with current ${hidden ? 'hidden' : 'visible'}`, async ({ page }) => {
		await library(page);
		await enqueue(page);
		if (hidden) await filter(page).fill('Second');
		await trim(page).click();
		await expect(yes(page)).toBeVisible();
		const shuffle = page.getByRole('button', { name: 'Shuffle upcoming', exact: true });
		await expect(shuffle).toBeEnabled();
		await shuffle.focus();
		await expect(shuffle).toBeFocused();
		await virtualConfirm(page);
		await expect(shuffle).toBeDisabled();
		await expect(page.getByTestId('queue-position')).toContainText('Track 1 of 1, 0 upcoming');
		await expectLiveRecovery(hidden ? page.getByTestId('queue-reveal-current') : current(page));
		if (hidden) {
			await expect(filter(page)).toHaveValue('Second');
			await expect(page.getByTestId('queue-item')).toHaveCount(0);
		}
	});
}

test('QCR-04 control: virtual all-clear preserves a still-enabled newer filter focus', async ({ page }) => {
	await library(page);
	await enqueue(page);
	await clear(page).click();
	await expect(yes(page)).toBeVisible();
	await filter(page).focus();
	await expect(filter(page)).toBeFocused();
	await virtualConfirm(page);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await flush(page);
	await expect(filter(page)).toBeEnabled();
	await expect(filter(page)).toBeFocused();
});

async function placeAtBottom(page: Page, target: Locator): Promise<void> {
	await target.focus();
	await target.evaluate((element) => {
		const box = element.getBoundingClientRect();
		window.scrollBy(0, box.bottom - (window.innerHeight - 4));
	});
	const box = await target.boundingBox();
	expect(box).not.toBeNull();
	expect(box!.y + box!.height).toBeGreaterThan((page.viewportSize()?.height ?? 0) - 12);
	await expect(target).toBeInViewport();
}

for (const width of [800, 390]) {
	test(`QCR-05 opening at ${width}x260 scrolls safe Cancel into the actual viewport`, async ({ page }) => {
		await page.setViewportSize({ width, height: 260 });
		// Extra unqueued library rows prevent page-bottom scroll clamping from
		// accidentally hiding a missing focus-recovery scroll.
		await library(page, [...ITEMS, ...Array.from({ length: 20 }, (_, index) => ({ id: `unqueued-${index}`, title: `Unqueued ${index}` }))]);
		await enqueue(page);
		await placeAtBottom(page, clear(page));
		await clear(page).press('Enter');
		await expect(cancel(page)).toBeFocused();
		await expect(cancel(page), 'autofocus is not enough if Cancel is offscreen').toBeInViewport();
	});

	test(`QCR-05 cancellation at ${width}x260 scrolls the above-viewport trigger back into view`, async ({ page }) => {
		await page.setViewportSize({ width, height: 260 });
		// Extra unqueued library rows prevent page-bottom scroll clamping from
		// accidentally hiding a missing focus-recovery scroll.
		await library(page, [...ITEMS, ...Array.from({ length: 20 }, (_, index) => ({ id: `unqueued-${index}`, title: `Unqueued ${index}` }))]);
		await enqueue(page);
		await clear(page).click();
		await expect(cancel(page)).toBeFocused();
		await cancel(page).scrollIntoViewIfNeeded();
		await cancel(page).evaluate((element) => window.scrollBy(0, element.getBoundingClientRect().top - 4));
		await expect(cancel(page)).toBeInViewport();
		expect((await clear(page).boundingBox())!.y + (await clear(page).boundingBox())!.height).toBeLessThan(0);
		// Keyboard activation does not call locator.click's helpful auto-scrolling.
		await cancel(page).press('Enter');
		await expect(yes(page)).toHaveCount(0);
		await expect(clear(page)).toBeFocused();
		await expect(clear(page), 'cancel must recover visible focus, not merely a connected trigger').toBeInViewport();
	});
}

test('QCR-06 revised upcoming scope describes Confirm and is announced politely or requires fresh consent', async ({ page }) => {
	await nativeAudio(page);
	const tracks = ITEMS.map((track) => ({ ...track, streamUrl: `/test-audio/${track.id}` }));
	await library(page, tracks);
	await enqueue(page, tracks);
	await trim(page).click();
	await expect(cancel(page)).toBeFocused();
	await expect.soft(yes(page), 'Confirm needs the count, not only Cancel').toHaveAccessibleDescription(/\b2\b.*upcoming|upcoming.*\b2\b/i, { timeout: 1000 });
	await expect.soft(yes(page)).toHaveAccessibleDescription(IRREVERSIBLE, { timeout: 1000 });
	const liveWarnings = page.locator('main [aria-live="polite"], main [role="status"]')
		.filter({ hasText: /upcoming/i }).filter({ hasText: /remov|clear/i });
	const beforeWarning = await liveWarnings.allTextContents();
	const oldYes = await yes(page).elementHandle();
	await yes(page).focus();
	// The explicit supported Next action changes the open scope, without moving
	// DOM focus itself. This is distinct from implicit library reconciliation.
	await page.getByTestId('player-next').evaluate((element: HTMLButtonElement) => element.click());
	await expect(current(page)).toHaveAttribute('data-track-id', 'consent-second');
	const retainedConsent = await oldYes!.evaluate((element: HTMLButtonElement) => element.isConnected && !element.disabled);
	if (retainedConsent) {
		await expect(yes(page)).toBeVisible();
		await expect.soft(yes(page)).toHaveAccessibleDescription(/\b1\b.*upcoming|upcoming.*\b1\b/i, { timeout: 1000 });
		await expect.soft(yes(page)).toHaveAccessibleDescription(IRREVERSIBLE, { timeout: 1000 });
		await expect(liveWarnings.filter({ hasText: /\b1\b/ }),
			'revised removal scope needs changed polite content, not a claim of actual screen-reader speech').not.toHaveCount(0);
		expect(await liveWarnings.allTextContents()).not.toEqual(beforeWarning);
		await expect(yes(page)).toBeFocused();
	} else {
		// A host may instead invalidate the old action and require a fresh choice.
		await oldYes!.evaluate((element: HTMLButtonElement) => element.click());
		await flush(page);
		expect(await order(page)).toEqual(['consent-first', 'consent-second', 'consent-third']);
		if (await yes(page).count() === 0 || !await yes(page).isEnabled()) await trim(page).click();
		await expect(cancel(page)).toBeFocused();
		await expect(yes(page)).toHaveAccessibleDescription(/\b1\b.*upcoming|upcoming.*\b1\b/i);
		await expect(yes(page)).toHaveAccessibleDescription(IRREVERSIBLE);
	}
	expect(await order(page)).toEqual(['consent-first', 'consent-second', 'consent-third']);
});

for (const hidden of [true, false]) {
	test(`QCR-08 narrow long-queue trim scrolls ${hidden ? 'hidden-current reveal' : 'visible current row'} recovery into the viewport`, async ({ page }) => {
		await page.setViewportSize({ width: 360, height: 260 });
		const previous = Array.from({ length: 24 }, (_, index) => ({ id: `archive-${index}`, title: `Archive ${index}` }));
		const selected = { id: 'long-current', title: 'Hidden Current' };
		const upcoming = Array.from({ length: 8 }, (_, index) => ({ id: `future-${index}`, title: `Future ${index}` }));
		const tracks = [...previous, selected, ...upcoming];
		await library(page, tracks);
		await enqueue(page, tracks);
		await queueItem(page, selected.id).getByRole('button', { name: 'Select Hidden Current', exact: true }).click();
		if (hidden) await filter(page).fill('  aRcHiVe  ');
		await trim(page).click();
		await expect(cancel(page)).toBeFocused();
		await yes(page).scrollIntoViewIfNeeded();
		await yes(page).focus();
		await expect(yes(page)).toBeInViewport();
		const target = hidden ? page.getByTestId('queue-reveal-current') : current(page);
		await expect(target, 'fixture must require actual recovery scrolling').not.toBeInViewport();
		await yes(page).press('Enter');
		await expect(yes(page)).toHaveCount(0);
		await expect(trim(page)).toBeDisabled();
		await expect(page.getByTestId('queue-position')).toContainText('Track 25 of 25, 0 upcoming');
		// Before any reveal click: focus alone/toBeVisible cannot prove this.
		await expect(target).toBeFocused();
		await expect(target).toBeInViewport();
		if (hidden) {
			await expect(filter(page)).toHaveValue('  aRcHiVe  ');
			expect(await order(page)).toEqual(previous.map((track) => track.id));
			await expect(current(page)).toHaveCount(0);
		} else expect(await order(page)).toEqual([...previous, selected].map((track) => track.id));
	});
}
