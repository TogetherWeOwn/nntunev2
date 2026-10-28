import { expect, test, type Page } from '@playwright/test';

// Builder-owned review regressions; the independent acceptance banks stay untouched.
// Real hydrated demo state and DOM; only the external media decoder is doubled.
const order = (page: Page) => page.getByTestId('queue-item').evaluateAll((rows) =>
	rows.map((row) => row.getAttribute('data-track-id')));
const filter = (page: Page) => page.getByRole('searchbox', { name: 'Filter queue', exact: true });
const announcement = (page: Page) => page.getByTestId('queue-announcement');

async function open(page: Page, baseURL: string) {
	const origin = new URL(baseURL);
	expect(['localhost', '127.0.0.1', '[::1]']).toContain(origin.hostname);
	await page.route('**/*', (route) => {
		const target = new URL(route.request().url());
		if (target.origin !== origin.origin) return route.abort();
		if (target.pathname === '/api/demo/audio') return;
		if (target.pathname.startsWith('/api/')) return route.abort();
		return route.continue();
	});
	await page.addInitScript(() => {
		const calls = { play: 0, pause: 0 };
		Object.assign(window, { removalMediaCalls: calls });
		HTMLMediaElement.prototype.play = function () {
			calls.play += 1;
			Object.defineProperty(this, 'paused', { value: false, configurable: true });
			return Promise.resolve();
		};
		HTMLMediaElement.prototype.pause = function () {
			calls.pause += 1;
			Object.defineProperty(this, 'paused', { value: true, configurable: true });
		};
	});
	await page.goto('/demo/player');
	// Prove hydration without creating queue feedback or changing its snapshot.
	const mute = page.getByRole('button', { name: 'Mute', exact: true });
	await mute.click();
	await expect(mute).toHaveAttribute('aria-pressed', 'true');
	await mute.click();
	await expect(mute).toHaveAttribute('aria-pressed', 'false');
}

test('removal commits only one emitting queue live region, including a filtered no-match result', async ({ page, baseURL }) => {
	await open(page, baseURL!);
	await expect(announcement(page)).toHaveText('');
	await expect(page.getByTestId('queue-panel').locator('[aria-live="off"]').filter({ hasText: '3 of 3 tracks' })).toHaveCount(1);
	await filter(page).fill('second');
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	await page.evaluate(() => {
		const panel = document.querySelector('[data-testid="queue-panel"]')!;
		const commits: string[] = [];
		Object.assign(window, { removalLiveCommits: commits });
		new MutationObserver((records) => {
			const changed = new Set<Element>();
			for (const record of records) {
				const node = record.target instanceof Element ? record.target : record.target.parentElement;
				const region = node?.closest('[role="status"], [aria-live]');
				if (region && region.getAttribute('aria-live') !== 'off') changed.add(region);
				// A newly inserted live region is also an announcement source.
				for (const added of record.addedNodes) {
					if (added instanceof Element && added.matches('[role="status"], [aria-live]') && added.getAttribute('aria-live') !== 'off') changed.add(added);
				}
			}
			for (const region of changed) commits.push(region.textContent?.trim() ?? '');
		}).observe(panel, { childList: true, characterData: true, subtree: true });
	});
	await page.getByTestId('queue-remove-second-signal').click();
	await expect(announcement(page)).toHaveText('Second Signal removed from the queue');
	const commits = await page.evaluate(() => (window as unknown as { removalLiveCommits: string[] }).removalLiveCommits);
	expect(commits).toEqual(['Second Signal removed from the queue']);
	// Filtering still announces counts and no matches through that same path.
	await filter(page).fill('unmatched');
	await expect(announcement(page)).toContainText('0 of 2 tracks');
	await expect(announcement(page)).toContainText('No matches in this queue');
	await page.getByRole('button', { name: 'Clear queue filter', exact: true }).click();
	await expect(announcement(page)).toContainText('2 of 2 tracks');
});

for (const repeated of [false, true]) {
	test(`same-turn removals resolve live track identity (${repeated ? 'same row twice' : 'two adjacent rows'})`, async ({ page, baseURL }) => {
		await open(page, baseURL!);
		await page.evaluate((repeat) => {
			const first = document.querySelector<HTMLButtonElement>('[data-testid="queue-remove-demo-track"]')!;
			const second = document.querySelector<HTMLButtonElement>('[data-testid="queue-remove-second-signal"]')!;
			first.click();
			(repeat ? first : second).click();
		}, repeated);
		await expect.poll(() => order(page)).toEqual(repeated ? ['second-signal', 'third-light'] : ['third-light']);
		await expect(page.getByTestId('now-playing')).toContainText(repeated ? 'Second Signal' : 'Third Light');
	});
}

test('same-turn selection and removal cancel pending playback before the DOM flush', async ({ page, baseURL }) => {
	await open(page, baseURL!);
	const plays = await page.evaluate(() => (window as unknown as { removalMediaCalls: { play: number } }).removalMediaCalls.play);
	expect(plays).toBe(0);
	await page.evaluate(async () => {
		// No awaited locator action between these activations: play is awaiting
		// Svelte's flush when removal must invalidate its transport generation.
		document.querySelector<HTMLButtonElement>('[data-testid="queue-select-second-signal"]')!.click();
		document.querySelector<HTMLButtonElement>('[data-testid="queue-remove-second-signal"]')!.click();
		await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
	});
	await expect.poll(() => order(page)).toEqual(['demo-track', 'third-light']);
	await expect(page.getByTestId('now-playing')).toHaveText('Selected: Third Light — Session Players');
	await expect(page.getByTestId('player-status')).toHaveText('Status: selected');
	expect(await page.evaluate(() => (window as unknown as { removalMediaCalls: { play: number } }).removalMediaCalls.play)).toBe(plays);
});

test('removing the still-current queue row leaves active radio selection and playback untouched', async ({ page, baseURL }) => {
	await open(page, baseURL!);
	await page.getByTestId('queue-select-third-light').click();
	await expect(page.getByTestId('now-playing')).toHaveText('Now playing: Third Light — Session Players');
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await page.getByTestId('demo-audio').evaluate((audio) => {
		Object.defineProperty(audio, 'paused', { value: true, configurable: true });
		Object.defineProperty(audio, 'ended', { value: true, configurable: true });
		audio.dispatchEvent(new Event('pause'));
		audio.dispatchEvent(new Event('ended'));
	});
	await expect(page.getByTestId('radio-badge')).toHaveText('Radio');
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	const selection = await page.getByTestId('now-playing').textContent();
	const audio = await page.getByTestId('demo-audio').elementHandle();
	const calls = await page.evaluate(() => ({ ...(window as unknown as { removalMediaCalls: { play: number; pause: number } }).removalMediaCalls }));
	expect(calls.play).toBeGreaterThan(0);
	await page.getByTestId('queue-remove-third-light').click();
	await expect.poll(() => order(page)).toEqual(['demo-track', 'second-signal']);
	await expect(page.getByTestId('radio-badge')).toHaveText('Radio');
	await expect(page.getByTestId('now-playing')).toHaveText(selection!);
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	expect(await audio!.evaluate((node) => node === document.querySelector('[data-testid="demo-audio"]') && !(node as HTMLAudioElement).paused)).toBe(true);
	expect(await page.evaluate(() => (window as unknown as { removalMediaCalls: { play: number; pause: number } }).removalMediaCalls)).toEqual(calls);
});

test('removed current audio cannot resurrect its resume position during teardown or re-add', async ({ page, baseURL }) => {
	await open(page, baseURL!);
	await page.evaluate(() => {
		localStorage.setItem('nntune:resume:v1', JSON.stringify({ 'demo-track': 25, 'second-signal': 12 }));
		Object.defineProperty(document.querySelector('[data-testid="demo-audio"]'), 'currentTime', { value: 45, writable: true, configurable: true });
	});
	await page.getByTestId('queue-remove-demo-track').click();
	await expect.poll(() => order(page)).toEqual(['second-signal', 'third-light']);
	expect(await page.evaluate(() => JSON.parse(localStorage.getItem('nntune:resume:v1') ?? '{}'))).toEqual({ 'second-signal': 12 });
	await page.getByTestId('queue-add-demo-track').click();
	await page.getByTestId('queue-select-demo-track').click();
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	await expect.poll(() => page.getByTestId('demo-audio').evaluate((node) => (node as HTMLAudioElement).currentTime)).toBe(0);
	// The fence belongs to the discarded element, not to this track forever.
	await page.getByTestId('demo-audio').evaluate((node) => Object.defineProperty(node, 'currentTime', { value: 18, writable: true, configurable: true }));
	await page.getByTestId('queue-select-third-light').click();
	expect(await page.evaluate(() => JSON.parse(localStorage.getItem('nntune:resume:v1') ?? '{}')['demo-track'])).toBe(18);
});

test('removing the last filtered row focuses the filter without revealing or deleting hidden tracks', async ({ page, baseURL }) => {
	await open(page, baseURL!);
	await filter(page).fill('second');
	await page.getByTestId('queue-remove-second-signal').focus();
	await page.keyboard.press('Enter');
	await expect(filter(page)).toBeFocused();
	await expect(filter(page)).toHaveValue('second');
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await page.getByRole('button', { name: 'Clear queue filter', exact: true }).click();
	await expect.poll(() => order(page)).toEqual(['demo-track', 'third-light']);
});

for (const boundary of ['null-active-element', 'blurred-document'] as const) {
	test(`removal does not recover focus across a ${boundary}`, async ({ page, baseURL }) => {
		await open(page, baseURL!);
		const calls = await page.evaluate(async (boundary) => {
			const button = document.querySelector<HTMLButtonElement>('[data-testid="queue-remove-second-signal"]')!;
			button.focus();
			const focus = HTMLElement.prototype.focus;
			const calls: string[] = [];
			HTMLElement.prototype.focus = function (options) {
				calls.push(this.getAttribute('data-testid') ?? this.tagName);
				focus.call(this, options);
			};
			if (boundary === 'null-active-element') {
				Object.defineProperty(document, 'activeElement', { configurable: true, get: () => button.isConnected ? button : null });
			} else {
				Object.defineProperty(document, 'hasFocus', { configurable: true, value: () => false });
			}
			try {
				button.click();
				await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
				return calls;
			} finally {
				Reflect.deleteProperty(document, boundary === 'null-active-element' ? 'activeElement' : 'hasFocus');
				HTMLElement.prototype.focus = focus;
			}
		}, boundary);
		expect(calls).toEqual([]);
		await expect.poll(() => order(page)).toEqual(['demo-track', 'third-light']);
	});
}

for (const via of ['add', 'recent', 'enqueue-recent'] as const) {
	test(`a play-next confirmation expires after re-adding changes the queue snapshot via ${via}`, async ({ page, baseURL }) => {
		await open(page, baseURL!);
		await page.getByTestId('queue-select-demo-track').click();
		await expect(page.getByTestId('recent-replay-demo-track')).toBeVisible();
		await page.getByTestId('queue-remove-demo-track').click();
		await page.getByTestId('queue-select-third-light').click();
		await page.getByTestId('queue-play-next-second-signal').click();
		await expect(announcement(page)).toHaveText('Second Signal moved to position 2 of 2');
		if (via === 'enqueue-recent') {
			await page.getByTestId('recently-played').getByRole('button', { name: 'Add Demo track to queue', exact: true }).click();
		} else {
			await page.getByTestId(via === 'add' ? 'queue-add-demo-track' : 'recent-replay-demo-track').click();
		}
		await expect.poll(() => order(page)).toEqual(['third-light', 'second-signal', 'demo-track']);
		await expect(announcement(page)).toHaveText('');
	});
}

test('an unrelated re-add preserves the latest removal confirmation', async ({ page, baseURL }) => {
	await open(page, baseURL!);
	await page.getByTestId('queue-remove-second-signal').click();
	await page.getByTestId('queue-remove-third-light').click();
	await expect(announcement(page)).toHaveText('Third Light removed from the queue');
	await page.getByTestId('queue-add-second-signal').click();
	await expect(announcement(page)).toHaveText('Third Light removed from the queue');
});

test('replaying an in-queue recent track preserves the removal confirmation', async ({ page, baseURL }) => {
	await open(page, baseURL!);
	await page.getByTestId('queue-select-demo-track').click();
	await expect(page.getByTestId('recent-replay-demo-track')).toBeVisible();
	await page.getByTestId('queue-remove-second-signal').click();
	await page.getByTestId('recent-replay-demo-track').click();
	await expect(announcement(page)).toHaveText('Second Signal removed from the queue');
});

// Deleting Clear's activation must fail even though input activation still works.
test('Clear as the first filter action activates live counts', async ({ page, baseURL }) => {
	await open(page, baseURL!);
	await expect(filter(page)).toHaveValue('');
	await expect(announcement(page)).toHaveText('');
	await page.getByRole('button', { name: 'Clear queue filter', exact: true }).click();
	await expect(announcement(page)).toHaveText('3 of 3 tracks');
	await expect(filter(page)).toBeFocused();
});

// An unconditional discard fences the surviving node when the source returns to B.
test('same-turn tail selection and removal preserves the surviving node resume writes', async ({ page, baseURL }) => {
	await open(page, baseURL!);
	await page.getByTestId('queue-select-second-signal').click();
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	const survivor = await page.getByTestId('demo-audio').elementHandle();
	await survivor!.evaluate((node) => Object.defineProperty(node, 'currentTime', { value: 18, writable: true, configurable: true }));
	const plays = await page.evaluate(() => (window as unknown as { removalMediaCalls: { play: number } }).removalMediaCalls.play);
	await page.evaluate(async () => {
		document.querySelector<HTMLButtonElement>('[data-testid="queue-select-third-light"]')!.click();
		document.querySelector<HTMLButtonElement>('[data-testid="queue-remove-third-light"]')!.click();
		await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
	});
	await expect(page.getByTestId('now-playing')).toHaveText('Selected: Second Signal — Fixture Band');
	await expect(page.getByTestId('player-status')).toHaveText('Status: selected');
	expect(await survivor!.evaluate((node) => node === document.querySelector('[data-testid="demo-audio"]'))).toBe(true);
	expect(await page.evaluate(() => (window as unknown as { removalMediaCalls: { play: number } }).removalMediaCalls.play)).toBe(plays);
	await page.getByTestId('queue-select-second-signal').click();
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await survivor!.evaluate((node) => { (node as HTMLAudioElement).currentTime = 45; });
	await page.evaluate(() => window.dispatchEvent(new Event('pagehide')));
	expect(await page.evaluate(() => JSON.parse(localStorage.getItem('nntune:resume:v1') ?? '{}')['second-signal'])).toBe(45);
	await page.getByTestId('queue-select-demo-track').click();
	expect(await page.evaluate(() => JSON.parse(localStorage.getItem('nntune:resume:v1') ?? '{}')['second-signal'])).toBe(45);
});

for (const via of ['add', 'recent', 'enqueue-recent'] as const) {
	test(`repeat removal announces once after re-adding via ${via}`, async ({ page, baseURL }) => {
		await open(page, baseURL!);
		await page.getByTestId('queue-select-second-signal').click();
		await expect(page.getByTestId('recent-replay-second-signal')).toBeVisible();
		await page.getByTestId('queue-remove-second-signal').click();
		await expect(announcement(page)).toHaveText('Second Signal removed from the queue');
		const calls = await page.evaluate(() => ({ ...(window as unknown as { removalMediaCalls: { play: number; pause: number } }).removalMediaCalls }));
		const audio = await page.getByTestId('demo-audio').elementHandle();
		if (via === 'enqueue-recent') {
			await page.getByTestId('recently-played').getByRole('button', { name: 'Add Second Signal to queue', exact: true }).click();
			expect(await page.evaluate(() => (window as unknown as { removalMediaCalls: { play: number; pause: number } }).removalMediaCalls)).toEqual(calls);
			expect(await audio!.evaluate((node) => node === document.querySelector('[data-testid="demo-audio"]'))).toBe(true);
			await expect(page.getByTestId('player-status')).toHaveText('Status: selected');
		} else {
			await page.getByTestId(via === 'add' ? 'queue-add-second-signal' : 'recent-replay-second-signal').click();
		}
		await expect(announcement(page)).toHaveText('');
		await page.evaluate(() => {
			const region = document.querySelector('[data-testid="queue-announcement"]')!;
			const commits: string[] = [];
			Object.assign(window, { repeatRemovalCommits: commits });
			new MutationObserver(() => commits.push(region.textContent?.trim() ?? ''))
				.observe(region, { childList: true, characterData: true, subtree: true });
		});
		await page.getByTestId('queue-remove-second-signal').click();
		await expect(announcement(page)).toHaveText('Second Signal removed from the queue');
		expect(await page.evaluate(() => (window as unknown as { repeatRemovalCommits: string[] }).repeatRemovalCommits))
			.toEqual(['Second Signal removed from the queue']);
	});
}

// A resting-queue guard before radioActive strands the ended source after the last removal.
test('radio advances after removal empties the resting queue', async ({ page, baseURL }) => {
	await open(page, baseURL!);
	await page.getByTestId('queue-select-third-light').click();
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await page.getByTestId('demo-audio').evaluate((audio) => {
		Object.defineProperty(audio, 'paused', { value: true, configurable: true });
		Object.defineProperty(audio, 'ended', { value: true, configurable: true });
		audio.dispatchEvent(new Event('pause'));
		audio.dispatchEvent(new Event('ended'));
	});
	await expect(page.getByTestId('radio-badge')).toHaveText('Radio');
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	for (const id of ['demo-track', 'second-signal', 'third-light']) {
		await page.getByTestId(`queue-remove-${id}`).click();
	}
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	for (let end = 0; end < 2; end += 1) {
		const source = await page.getByTestId('demo-audio').getAttribute('src');
		const plays = await page.evaluate(() => (window as unknown as { removalMediaCalls: { play: number } }).removalMediaCalls.play);
		await page.getByTestId('demo-audio').evaluate((audio) => {
			Object.defineProperty(audio, 'paused', { value: true, configurable: true });
			Object.defineProperty(audio, 'ended', { value: true, configurable: true });
			audio.dispatchEvent(new Event('pause'));
			audio.dispatchEvent(new Event('ended'));
		});
		await expect(page.getByTestId('demo-audio')).not.toHaveAttribute('src', source!);
		await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
		await expect(page.getByTestId('radio-badge')).toHaveText('Radio');
		expect(await page.evaluate(() => (window as unknown as { removalMediaCalls: { play: number } }).removalMediaCalls.play)).toBe(plays + 1);
	}
});
