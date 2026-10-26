import { expect, test, type Page } from '@playwright/test';

/**
 * Builder-owned supplements for the library native restart control.
 *
 * These cases lock behavior the protected acceptance bank
 * (`tests/library-native-restart.spec.ts`) does not reach: queue-panel
 * edits that change the current source without going through the shared
 * queue-replacement path must still clear the restart outcome and its
 * availability snapshot, and a long track title must keep the outcome
 * under 100 characters. Same harness shape as the protected bank: the
 * real `/library` route with `/api/library` fulfilled in-test and
 * `/test-audio/**` held pending. No real decoder, no network media.
 */

test.setTimeout(30_000);

type Item = { id: string; title: string; artist?: string; streamUrl?: string };

const ITEMS: Item[] = [
	{ id: 'rb-coral', title: 'Coral Night', artist: 'Mira', streamUrl: '/test-audio/rb-coral' },
	{ id: 'rb-pine', title: 'Pine Morning', artist: 'North', streamUrl: '/test-audio/rb-pine' }
];
const LONG_TITLE = 'A very long title that would otherwise push the restart outcome far beyond its budget line';
const LONG_ITEMS: Item[] = [{ id: 'rb-long', title: LONG_TITLE, streamUrl: '/test-audio/rb-long' }];

const audio = (page: Page) => page.getByTestId('audio-player');
const restartButton = (page: Page) => page.getByTestId('library-restart');
const restartStatus = (page: Page) => page.getByTestId('library-restart-status');
const row = (page: Page, id: string) => page.locator(`[data-testid="library-item"][data-id="${id}"]`);
const queueRow = (page: Page, id: string) =>
	page.locator(`[data-testid="queue-item"][data-track-id="${id}"]`);

async function openLibrary(page: Page, items: Item[] = ITEMS) {
	await page.route('**/api/library', (route) =>
		route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(items) })
	);
	await page.route('**/test-audio/**', () => {});
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(items.length);
}

async function selectRow(page: Page, id: string, title: string) {
	const escaped = title.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
	await row(page, id)
		.getByRole('button', { name: new RegExp(`^${escaped}( — |$)`) })
		.click();
}

async function mockMedia(page: Page, time: number, duration = 120) {
	await audio(page).evaluate(
		(el: HTMLAudioElement, s: { time: number; duration: number }) => {
			let current = s.time;
			Object.defineProperty(el, 'duration', { value: s.duration, configurable: true });
			Object.defineProperty(el, 'currentTime', {
				get: () => current,
				set: (next: number) => {
					current = next;
				},
				configurable: true
			});
			Object.defineProperty(el, 'seekable', {
				get: () => ({ length: 1, start: () => 0, end: () => s.duration }),
				configurable: true
			});
			Object.defineProperty(el, 'readyState', { value: 4, configurable: true });
		},
		{ time, duration }
	);
}

async function signalMedia(page: Page) {
	await audio(page).evaluate((el: HTMLAudioElement) => {
		for (const name of ['loadedmetadata', 'durationchange', 'canplay', 'progress', 'timeupdate']) {
			el.dispatchEvent(new Event(name));
		}
	});
}

async function restartCoral(page: Page) {
	await selectRow(page, 'rb-coral', 'Coral Night');
	await selectRow(page, 'rb-pine', 'Pine Morning');
	await expect(page.getByTestId('now-playing')).toContainText('Coral Night');
	await mockMedia(page, 30);
	await signalMedia(page);
	await restartButton(page).click();
	await expect(restartStatus(page)).toContainText('Restarted Coral Night');
	await expect(restartButton(page)).toBeEnabled();
}

test('selecting another queue row clears the restart outcome and availability', async ({ page }) => {
	await openLibrary(page);
	await restartCoral(page);

	await queueRow(page, 'rb-pine').getByRole('button', { name: 'Select Pine Morning' }).click();
	await expect(page.getByTestId('now-playing')).toContainText('Pine Morning');

	await expect(restartStatus(page)).toHaveText('');
	await expect(restartButton(page)).toBeDisabled();
});

test('removing the current queue row clears the restart outcome and availability', async ({ page }) => {
	await openLibrary(page);
	await restartCoral(page);

	await queueRow(page, 'rb-coral').getByRole('button', { name: 'Remove Coral Night from queue' }).click();
	await expect(page.getByTestId('now-playing')).toContainText('Pine Morning');

	await expect(restartStatus(page)).toHaveText('');
	await expect(restartButton(page)).toBeDisabled();
});

test('a long track title keeps the restart outcome under 100 characters', async ({ page }) => {
	expect(LONG_TITLE.length).toBeGreaterThan(80);
	await openLibrary(page, LONG_ITEMS);
	await selectRow(page, 'rb-long', LONG_TITLE);
	await mockMedia(page, 30);
	await signalMedia(page);

	await restartButton(page).click();
	const text = ((await restartStatus(page).textContent()) ?? '').replace(/\s+/g, ' ').trim();
	expect(text).toMatch(/^Restarted /);
	expect(text.length).toBeLessThan(100);
	expect(text.length).toBeGreaterThan(0);
});
