import { expect, test, type Page } from '@playwright/test';

// Supplemental regression coverage; the independently authored acceptance bank
// stays unchanged. All library and media inputs are synthetic and local.
const ITEMS = [
	{ id: 'coral', title: 'Coral Night', streamUrl: '/test-audio/coral' },
	{ id: 'pine', title: 'Pine Morning', streamUrl: '/test-audio/pine' }
];

async function openLink(page: Page, { waitForMetadata = false } = {}) {
	await page.route('**/api/library', (route) => route.fulfill({ json: ITEMS }));
	await page.route('**/test-audio/**', () => {});
	await page.addInitScript((waitForMetadata) => {
		const time = Object.getOwnPropertyDescriptor(HTMLMediaElement.prototype, 'currentTime')!;
		Object.defineProperty(HTMLMediaElement.prototype, 'currentTime', {
			configurable: true,
			get: time.get,
			set(value: number) {
				this.dataset.seekAttempts = String(Number(this.dataset.seekAttempts ?? 0) + 1);
				if (waitForMetadata && this.dataset.seekReady !== 'yes') {
					throw new DOMException('Metadata not ready', 'InvalidStateError');
				}
				time.set!.call(this, value);
			}
		});
		Object.defineProperty(HTMLMediaElement.prototype, 'duration', {
			configurable: true,
			get: () => NaN
		});
		HTMLMediaElement.prototype.play = function () {
			this.dataset.playCalls = String(Number(this.dataset.playCalls ?? 0) + 1);
			return Promise.resolve();
		};
	}, waitForMetadata);
	await page.goto('/library?track=coral&t=90');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(page.getByTestId('audio-player')).toHaveCount(1);
	await expect(page.locator('[data-testid="queue-item"][aria-current="true"]')).toHaveAttribute(
		'data-track-id',
		'coral'
	);
	// Ensure the initial post-mount attempt happened before dispatching events.
	await expect
		.poll(() => page.getByTestId('audio-player').getAttribute('data-seek-attempts'))
		.not.toBeNull();
}

function audioTime(page: Page) {
	return page.getByTestId('audio-player').evaluate((element: HTMLAudioElement) => element.currentTime);
}

async function expectNoAutoplay(page: Page) {
	await expect(page.getByTestId('audio-player')).toHaveJSProperty('paused', true);
	expect(await page.getByTestId('audio-player').getAttribute('data-play-calls')).toBeNull();
}

for (const query of ['track=pine&track=coral&t=9', 'track=pine&t=9&t=10']) {
	test(`duplicate parameters reject atomically on same-route navigation: ${query}`, async ({ page }) => {
		await openLink(page);
		await expect.poll(() => audioTime(page)).toBe(90);
		await page.getByTestId('audio-player').evaluate((element) => {
			(element as HTMLElement).dataset.originalSource = 'yes';
		});
		// Click a real same-origin anchor so SvelteKit runs its afterNavigate hook
		// without reloading and discarding the existing queue/player state.
		await page.evaluate((query) => {
			const link = document.createElement('a');
			link.id = 'lifecycle-navigation';
			link.href = `/library?${query}`;
			link.textContent = 'Open duplicate link';
			document.body.append(link);
		}, query);
		await page.locator('#lifecycle-navigation').click();
		await expect(page.getByTestId('library-share-invalid')).toBeVisible();
		await expect(page.getByTestId('library-share-invalid')).toHaveAttribute('role', 'alert');
		await expect(page.getByTestId('queue-item')).toHaveCount(1);
		await expect(page.locator('[data-testid="queue-item"][aria-current="true"]')).toHaveAttribute(
			'data-track-id',
			'coral'
		);
		await expect(page.getByTestId('audio-player')).toHaveAttribute('data-original-source', 'yes');
		expect(await audioTime(page)).toBe(90);
		await expectNoAutoplay(page);
	});
}

test('loadedmetadata retries a refused initial seek on the matching source without autoplay', async ({
	page
}) => {
	await openLink(page, { waitForMetadata: true });
	expect(await audioTime(page)).toBe(0);
	await page.getByTestId('audio-player').evaluate((element: HTMLAudioElement) => {
		element.dataset.seekReady = 'yes';
		Object.defineProperty(element, 'duration', { configurable: true, value: 60 });
		element.dispatchEvent(new Event('loadedmetadata'));
	});
	await expect.poll(() => audioTime(page)).toBe(60);
	await expectNoAutoplay(page);
});

test('durationchange clamps an already applied unknown-duration seek once', async ({ page }) => {
	await openLink(page);
	await expect.poll(() => audioTime(page)).toBe(90);
	await page.getByTestId('audio-player').evaluate((element: HTMLAudioElement) => {
		Object.defineProperty(element, 'duration', { configurable: true, value: 60 });
		element.dispatchEvent(new Event('durationchange'));
	});
	await expect.poll(() => audioTime(page)).toBe(60);
	// A completed pending seek must not replay on later duration events.
	await page.getByTestId('audio-player').evaluate((element: HTMLAudioElement) => {
		element.currentTime = 12;
		element.dispatchEvent(new Event('durationchange'));
	});
	expect(await audioTime(page)).toBe(12);
	await expectNoAutoplay(page);
});

for (const event of ['loadedmetadata', 'durationchange']) {
	test(`listener seeking wins over a pending late clamp on ${event}`, async ({ page }) => {
		await openLink(page);
		await expect.poll(() => audioTime(page)).toBe(90);
		await page.getByTestId('audio-player').evaluate((element: HTMLAudioElement) => {
			element.currentTime = 10;
			element.dispatchEvent(new Event('seeking'));
		});
		expect(await audioTime(page)).toBe(10);
		await page.getByTestId('audio-player').evaluate((element: HTMLAudioElement, event) => {
			Object.defineProperty(element, 'duration', { configurable: true, value: 60 });
			element.dispatchEvent(new Event(event));
		}, event);
		expect(await audioTime(page)).toBe(10);
		await expectNoAutoplay(page);
	});
}
