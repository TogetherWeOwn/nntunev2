import { expect, test, type Page } from '@playwright/test';

/**
 * Supplemental Test-Author-owned library repeat acceptance. Keep the original
 * repeat-end-policy banks unchanged. All API/media traffic stays synthetic.
 *
 * Behavioral breaks pinned here: announcing before play resolves, leaving native
 * audio playing across auth teardown, retaining manual-pause intent, resetting
 * native settings on completion, and restoring only Queue (but not One).
 */
const ITEMS = [
	{ id: 'lib-coral', title: 'Coral Night', artist: 'Mira', streamUrl: '/test-audio/lib-coral' },
	{ id: 'lib-pine', title: 'Pine Morning', artist: 'North', streamUrl: '/test-audio/lib-pine' },
	{ id: 'lib-ember', title: 'Ember Line', artist: 'East', streamUrl: '/test-audio/lib-ember' }
];

type Mode = 'off' | 'one' | 'queue';
type PlayRequest = {
	src: string | null;
	time: number;
	volume: number;
	muted: boolean;
	rate: number;
	connected: boolean;
};
type Probe = {
	plays: PlayRequest[];
	deferAutomatic: boolean;
	pending: boolean;
	settle: ((outcome: 'resolve' | 'reject') => void) | null;
	unhandled: string[];
};
type ProbeWindow = Window & { __repeatLifecycle: Probe };

async function openLibrary(page: Page, deferAutomatic = false) {
	await page.route('**/api/library', (route) =>
		route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(ITEMS) })
	);
	await page.route('**/test-audio/**', () => {});
	await page.addInitScript((defer) => {
		const probe: Probe = {
			plays: [], deferAutomatic: defer, pending: false, settle: null, unhandled: []
		};
		(window as unknown as ProbeWindow).__repeatLifecycle = probe;
		window.addEventListener('unhandledrejection', (event) => {
			probe.unhandled.push(String(event.reason));
		});
		// Native play changes state and emits events even when its promise is
		// pending. Only the automatic (second) request is controllably deferred.
		HTMLMediaElement.prototype.play = function () {
			probe.plays.push({
				src: this.getAttribute('src'), time: this.currentTime,
				volume: this.volume, muted: this.muted, rate: this.playbackRate,
				connected: this.isConnected
			});
			Object.defineProperty(this, 'paused', { value: false, configurable: true });
			Object.defineProperty(this, 'ended', { value: false, configurable: true });
			this.dispatchEvent(new Event('play'));
			this.dispatchEvent(new Event('playing'));
			if (probe.deferAutomatic && probe.plays.length === 2) {
				probe.pending = true;
				return new Promise<void>((resolve, reject) => {
					probe.settle = (outcome) => {
						probe.pending = false;
						probe.settle = null;
						if (outcome === 'reject') reject(new DOMException('Synthetic autoplay denied', 'NotAllowedError'));
						else resolve();
					};
				});
			}
			return Promise.resolve();
		};
		HTMLMediaElement.prototype.pause = function () {
			Object.defineProperty(this, 'paused', { value: true, configurable: true });
			this.dispatchEvent(new Event('pause'));
		};
	}, deferAutomatic);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(3);
}

async function enqueue(page: Page, ids = ['lib-coral', 'lib-pine', 'lib-ember']) {
	for (const id of ids) {
		const item = ITEMS.find((item) => item.id === id)!;
		await page.locator(`[data-testid="library-item"][data-id="${id}"]`)
			.getByRole('button', { name: new RegExp(`^${item.title}( — |$)`) }).click();
	}
	await expect(page.getByTestId('queue-item')).toHaveCount(ids.length);
}

async function setMode(page: Page, mode: Mode) {
	await page.getByTestId('player-repeat').selectOption(mode);
	await expect(page.getByTestId('repeat-status')).toContainText(
		new RegExp(`Repeat:\\s*${mode}`, 'i')
	);
}

async function playCurrent(page: Page) {
	await page.getByTestId('audio-player').evaluate(async (element) => {
		await (element as HTMLMediaElement).play();
	});
	await expect.poll(() => plays(page).then((requests) => requests.length)).toBe(1);
	await expect(page.getByTestId('audio-player')).toHaveJSProperty('paused', false);
	await expect(page.getByTestId('audio-player')).toHaveJSProperty('ended', false);
}

async function completeCurrent(page: Page) {
	await page.getByTestId('audio-player').evaluate((element) => {
		Object.defineProperty(element, 'paused', { value: true, configurable: true });
		Object.defineProperty(element, 'ended', { value: true, configurable: true });
		element.dispatchEvent(new Event('pause'));
		element.dispatchEvent(new Event('ended'));
	});
}

async function plays(page: Page): Promise<PlayRequest[]> {
	return page.evaluate(() => (window as unknown as ProbeWindow).__repeatLifecycle.plays);
}

async function settleFrames(page: Page) {
	await page.evaluate(() => new Promise<void>((resolve) => {
		requestAnimationFrame(() => requestAnimationFrame(() => resolve()));
	}));
}

async function settleAutomatic(page: Page, outcome: 'resolve' | 'reject') {
	await page.evaluate((result) => {
		const probe = (window as unknown as ProbeWindow).__repeatLifecycle;
		if (!probe.pending || !probe.settle) throw new Error('No automatic play is pending');
		probe.settle(result);
	}, outcome);
	await settleFrames(page);
}

async function expectNoPlaying(page: Page, soft = false) {
	// Auth teardown may unmount the region. In either case no stale outcome may
	// survive; inspecting all texts avoids waiting for a deliberately removed node.
	const texts = await page.getByTestId('queue-announcement').allTextContents();
	const check = soft ? expect.soft : expect;
	check(texts.map((text) => text.trim()).filter(Boolean),
		'No premature/stale Playing announcement').toEqual([]);
}

async function queueOrder(page: Page) {
	return page.getByTestId('queue-item').evaluateAll((elements) =>
		elements.map((element) => element.getAttribute('data-track-id') ?? element.getAttribute('data-id'))
	);
}

async function expectNoErrors(page: Page, errors: string[]) {
	expect.soft(errors, 'No pageerror from asynchronous playback').toEqual([]);
	expect.soft(await page.evaluate(() => (window as unknown as ProbeWindow).__repeatLifecycle.unhandled),
		'No unhandled rejection from asynchronous playback').toEqual([]);
}

test('pending automatic play is silent and auth teardown pauses its connected native audio', async ({ page }) => {
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(error.message));
	await openLibrary(page, true);
	await enqueue(page);
	await setMode(page, 'one');
	await playCurrent(page);
	const audio = await page.getByTestId('audio-player').elementHandle();
	expect(audio).not.toBeNull();
	expect(await audio!.evaluate((element) => element.isConnected)).toBe(true);
	await completeCurrent(page);
	await expect.poll(() => plays(page).then((requests) => requests.length)).toBe(2);
	await settleFrames(page);
	expect(await page.evaluate(() => (window as unknown as ProbeWindow).__repeatLifecycle.pending)).toBe(true);
	expect(await audio!.evaluate((element) => element === document.querySelector('[data-testid="audio-player"]'))).toBe(true);
	expect(await audio!.evaluate((element) => (element as HTMLMediaElement).paused)).toBe(false);
	await expectNoPlaying(page);

	// Use a genuine cross-tab storage event, not a synthetic detached ended.
	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate(() => {
		localStorage.setItem('nntune.library.auth-transition.v1', 'synthetic-session-change');
		localStorage.removeItem('nntune.library.auth-transition.v1');
	});
	await otherTab.close();
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	expect(await audio!.evaluate((element) => (element as HTMLMediaElement).paused),
		'Auth boundary must pause the active native element, even with play pending').toBe(true);
	await settleAutomatic(page, 'resolve');
	await expectNoPlaying(page);
	expect(await plays(page)).toHaveLength(2);
	await expect(page.getByRole('button', { name: 'Retry', exact: true })).toBeVisible();
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(3);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
	await expectNoPlaying(page);
	expect(await plays(page)).toHaveLength(2);
	await expectNoErrors(page, errors);
});

test('rejected automatic play never announces Playing or escapes as an unhandled rejection', async ({ page }) => {
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(error.message));
	await openLibrary(page, true);
	await enqueue(page);
	await setMode(page, 'one');
	await playCurrent(page);
	await completeCurrent(page);
	await expect.poll(() => plays(page).then((requests) => requests.length)).toBe(2);
	await settleFrames(page);
	// Soft silence check ensures the rejection still executes under an early
	// announcement bug, so both the outcome and unhandled rejection are pinned.
	await expectNoPlaying(page, true);
	await settleAutomatic(page, 'reject');
	await expectNoErrors(page, errors);
	await expectNoPlaying(page);
	expect(await plays(page)).toHaveLength(2);
	await expect(page.getByTestId('now-playing')).toContainText('Coral Night');
	expect(await queueOrder(page)).toEqual(['lib-coral', 'lib-pine', 'lib-ember']);
});

for (const mode of ['off', 'one', 'queue'] as const) {
	test(`manual pause cancels intent before otherwise valid completion under ${mode}`, async ({ page }) => {
		await openLibrary(page);
		await enqueue(page);
		await setMode(page, mode);
		await playCurrent(page);
		const audio = await page.getByTestId('audio-player').elementHandle();
		expect(audio).not.toBeNull();
		await audio!.evaluate((element) => (element as HTMLMediaElement).pause());
		expect(await audio!.evaluate((element) => ({
			connected: element.isConnected,
			paused: (element as HTMLMediaElement).paused,
			ended: (element as HTMLMediaElement).ended
		}))).toEqual({ connected: true, paused: true, ended: false });
		// End becomes valid only AFTER the manual pause cleared playback intent.
		await completeCurrent(page);
		await settleFrames(page);
		expect(await plays(page), 'Manual pause must suppress the automatic play request').toHaveLength(1);
		await expect(page.getByTestId('now-playing')).toContainText('Coral Night');
		expect(await audio!.evaluate((element) => element === document.querySelector('[data-testid="audio-player"]'))).toBe(true);
		expect(await audio!.evaluate((element) => (element as HTMLMediaElement).ended)).toBe(true);
		expect(await queueOrder(page)).toEqual(['lib-coral', 'lib-pine', 'lib-ember']);
		await expectNoPlaying(page);
	});
}

for (const scenario of [
	{ name: 'Off advancement', mode: 'off', tail: false, singleton: false, expected: ITEMS[1]! },
	{ name: 'One replay', mode: 'one', tail: false, singleton: false, expected: ITEMS[0]! },
	{ name: 'Queue wrap', mode: 'queue', tail: true, singleton: false, expected: ITEMS[0]! },
	{ name: 'Queue singleton replay', mode: 'queue', tail: false, singleton: true, expected: ITEMS[1]! }
] as const) {
	test(`${scenario.name} preserves volume, mute and rate while playing from zero`, async ({ page }) => {
		await openLibrary(page);
		await enqueue(page, scenario.singleton ? ['lib-pine'] : undefined);
		await setMode(page, scenario.mode);
		if (scenario.tail) {
			await page.getByTestId('player-next').click();
			await page.getByTestId('player-next').click();
			await expect(page.getByTestId('now-playing')).toContainText('Ember Line');
		}
		await playCurrent(page);
		await page.getByTestId('audio-player').evaluate((element) => {
			const audio = element as HTMLMediaElement;
			audio.volume = 0.4;
			audio.muted = true;
			audio.playbackRate = 1.5;
			audio.currentTime = 37;
			audio.dispatchEvent(new Event('volumechange'));
			audio.dispatchEvent(new Event('ratechange'));
		});
		expect(await page.getByTestId('audio-player').evaluate((element) => {
			const audio = element as HTMLMediaElement;
			return { volume: audio.volume, muted: audio.muted, rate: audio.playbackRate, time: audio.currentTime };
		}), 'Completion starts with observable nondefault settings and a nonzero position')
			.toEqual({ volume: 0.4, muted: true, rate: 1.5, time: 37 });
		await completeCurrent(page);
		await expect.poll(() => plays(page).then((requests) => requests.length)).toBe(2);
		await expect(page.getByTestId('now-playing')).toContainText(scenario.expected.title);
		await expect(page.getByTestId('audio-player')).toHaveAttribute('src', scenario.expected.streamUrl);
		const requests = await plays(page);
		expect(requests[1], 'Automatic play must receive preserved native settings and zero position').toEqual({
			src: scenario.expected.streamUrl, time: 0, volume: 0.4, muted: true, rate: 1.5, connected: true
		});
		expect(await page.getByTestId('audio-player').evaluate((element) => {
			const audio = element as HTMLMediaElement;
			return { volume: audio.volume, muted: audio.muted, rate: audio.playbackRate, time: audio.currentTime };
		})).toEqual({ volume: 0.4, muted: true, rate: 1.5, time: 0 });
		expect(await queueOrder(page)).toEqual(scenario.singleton ? ['lib-pine'] : ['lib-coral', 'lib-pine', 'lib-ember']);
		await expect(page.getByTestId('queue-announcement')).toContainText(scenario.expected.title);
	});
}

test('persisted One restores both select and status and still replays on completion', async ({ page }) => {
	await openLibrary(page);
	await setMode(page, 'one');
	await page.reload();
	await expect(page.getByTestId('library-item')).toHaveCount(3);
	await settleFrames(page);
	expect(await page.getByTestId('player-repeat').inputValue(), 'Reload restores One, not Off').toBe('one');
	await expect(page.getByTestId('repeat-status')).toContainText(/Repeat:\s*One/);
	await enqueue(page);
	await playCurrent(page);
	await page.getByTestId('audio-player').evaluate((element) => {
		(element as HTMLMediaElement).currentTime = 19;
	});
	expect(await page.getByTestId('audio-player').evaluate((element) => (element as HTMLMediaElement).currentTime))
		.toBe(19);
	await completeCurrent(page);
	await expect.poll(() => plays(page).then((requests) => requests.length)).toBe(2);
	await expect(page.getByTestId('now-playing')).toContainText('Coral Night');
	expect((await plays(page))[1]).toMatchObject({ src: '/test-audio/lib-coral', time: 0, connected: true });
});

test('persisted Queue round-trips and corruption of its actual storage key falls back to Off', async ({ page }) => {
	await openLibrary(page);
	await setMode(page, 'queue');
	await page.reload();
	await expect(page.getByTestId('library-item')).toHaveCount(3);
	await expect(page.getByTestId('player-repeat')).toHaveValue('queue');
	await expect(page.getByTestId('repeat-status')).toContainText(/Repeat:\s*Queue/);
	const entries = await page.evaluate(() => {
		const entries: { store: 'localStorage' | 'sessionStorage'; key: string; value: string | null }[] = [];
		for (const store of ['localStorage', 'sessionStorage'] as const) {
			for (const key of Object.keys(window[store])) {
				const value = window[store].getItem(key);
				if (/repeat/i.test(key) && /^(queue|"queue")$/.test(value ?? '')) entries.push({ store, key, value });
			}
		}
		return entries;
	});
	expect(entries, 'A real persisted repeat key must exist; never skip the corruption check').toHaveLength(1);
	const entry = entries[0]!;
	expect(entry.value).not.toBeNull();
	await page.evaluate(({ store, key }) => {
		if (window[store].getItem(key) === null) throw new Error('Repeat key disappeared before corruption');
		window[store].setItem(key, '{"mode":"bogus"');
	}, entry);
	await page.reload();
	await expect(page.getByTestId('library-item')).toHaveCount(3);
	await expect(page.getByTestId('player-repeat')).toHaveValue('off');
	await expect(page.getByTestId('repeat-status')).toContainText(/Repeat:\s*Off/);
});
