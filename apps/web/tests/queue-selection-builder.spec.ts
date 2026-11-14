/** Real-media interaction coverage for the production queue selection owner. */
import { Buffer } from 'node:buffer';
import AxeBuilder from '@axe-core/playwright';
import { expect, test, type Page } from '@playwright/test';
import { openQueue } from './helpers/queue-host.js';
import { seekProofFailure, seekViaPlayerUi } from '../../../e2e/helpers/player-ui.js';

const ITEMS = ['a', 'b', 'c', 'd'].map((id) => ({
	id: `batch-${id}`, title: `Batch ${id.toUpperCase()}`, artist: 'Generated fixture', streamUrl: `/test-audio/batch-${id}`
}));
const row = (page: Page, id: string) => page.locator(`[data-testid="queue-item"][data-track-id="batch-${id}"]`);
const pick = (page: Page, id: string) => row(page, id).getByRole('checkbox');
const actions = (page: Page) => page.getByTestId('queue-selection-toolbar');

async function setup(page: Page, playing = false) {
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(error.message));
	await page.route('**/api/library', (route) => route.fulfill({ json: ITEMS }));
	const bytes = 90 * 8000 * 2;
	const wav = Buffer.alloc(44 + bytes);
	wav.write('RIFF'); wav.writeUInt32LE(36 + bytes, 4); wav.write('WAVEfmt ', 8);
	wav.writeUInt32LE(16, 16); wav.writeUInt16LE(1, 20); wav.writeUInt16LE(1, 22);
	wav.writeUInt32LE(8000, 24); wav.writeUInt32LE(16000, 28);
	wav.writeUInt16LE(2, 32); wav.writeUInt16LE(16, 34);
	wav.write('data', 36); wav.writeUInt32LE(bytes, 40);
	const requests: string[] = [];
	await page.route('**/test-audio/**', (route) => {
		requests.push(route.request().url());
		const range = /^bytes=(\d+)-(\d*)$/.exec(route.request().headers()['range'] ?? '');
		const start = range ? Number(range[1]) : 0;
		const end = range?.[2] ? Math.min(Number(range[2]), wav.length - 1) : wav.length - 1;
		if (start > end) return route.fulfill({ status: 416, headers: { 'Content-Range': `bytes */${wav.length}` } });
		return route.fulfill({ status: range ? 206 : 200, contentType: 'audio/wav',
			headers: { 'Accept-Ranges': 'bytes', ...(range ? { 'Content-Range': `bytes ${start}-${end}/${wav.length}` } : {}) },
			body: wav.subarray(start, end + 1) });
	});
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	for (const item of ITEMS) {
		await page.locator(`[data-testid="library-item"][data-id="${item.id}"]`)
			.getByRole('button', { name: new RegExp(`^${item.title} —`) }).click();
	}
	const audio = page.getByTestId('audio-player');
	await expect(audio).toHaveJSProperty('readyState', 4);
	if (!(await audio.evaluate((el: HTMLAudioElement) => el.paused))) await page.getByTestId('player-play-pause').click();
	await audio.evaluate((el: HTMLAudioElement) => {
		el.dataset.batchSeeked = 'false';
		el.addEventListener('seeked', () => { el.dataset.batchSeeked = 'true'; }, { once: true });
	});
	// The seek slider lives in the expanded route at every width.
	await page.getByTestId('player-now-playing-toggle').click();
	await expect(page.getByTestId('now-playing-route')).toBeVisible();
	await seekViaPlayerUi(page, audio, 0.35);
	await page.getByRole('button', { name: 'Close Now playing', exact: true }).click();
	await expect(page).toHaveURL(/\/library$/);
	await expect(audio).toHaveAttribute('data-batch-seeked', 'true');
	await expect(audio).toHaveJSProperty('seeking', false);
	const sought = await audio.evaluate((el: HTMLAudioElement) => ({ currentTime: el.currentTime, duration: el.duration, paused: el.paused }));
	expect(seekProofFailure(sought, 0.35, true)).toBeNull();
	expect(sought.currentTime).toBeGreaterThan(20);
	if (playing) await page.getByTestId('player-play-pause').click();
	await expect(audio).toHaveJSProperty('paused', !playing);
	if (playing) await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBeGreaterThan(sought.currentTime + 0.2);
	await openQueue(page);
	await expect(page.getByTestId('queue-item')).toHaveCount(ITEMS.length);
	return { audio, errors, requests };
}

for (const width of [390, 1280]) {
	for (const playing of [false, true]) {
		test(`hidden bulk edits preserve the real ${playing ? 'playing' : 'paused'} decoder at ${width}px`, async ({ page }) => {
			await page.setViewportSize({ width, height: 900 });
			const { audio, errors, requests } = await setup(page, playing);
			const original = (await audio.elementHandle())!;
			const before = await audio.evaluate((el: HTMLAudioElement) => {
				const events: string[] = [];
				for (const name of ['play', 'pause', 'emptied', 'loadstart', 'seeking', 'seeked']) el.addEventListener(name, () => events.push(name));
				Object.assign(el, { batchEvents: events });
				return el.currentTime;
			});
			await pick(page, 'd').click();
			await pick(page, 'b').click();
			await page.getByLabel('Filter queue', { exact: true }).fill('Batch B');
			await expect(actions(page)).toContainText('2 selected');
			await expect(page.getByText('1 selected track is hidden by this filter.', { exact: true })).toBeVisible();
			await actions(page).getByRole('button', { name: 'Play next', exact: true }).click();
			await expect(page.getByTestId('queue-announcement')).toHaveText('Moved 2 tracks to play next.');
			expect(await original.evaluate((el) => el === document.querySelector('[data-testid="audio-player"]'))).toBe(true);
			await expect(audio).toHaveAttribute('src', ITEMS[0].streamUrl);
			await expect(audio).toHaveJSProperty('paused', !playing);
			if (playing) await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBeGreaterThan(before);
			else expect(await audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBe(before);
			expect(await audio.evaluate((el) => (el as HTMLAudioElement & { batchEvents: string[] }).batchEvents)).toEqual([]);
			await page.getByRole('button', { name: 'Clear queue filter', exact: true }).click();
			await expect(page.getByTestId('queue-item')).toHaveCount(4);
			expect(await page.getByTestId('queue-item').evaluateAll((rows) => rows.map((el) => el.getAttribute('data-track-id'))))
				.toEqual(['batch-a', 'batch-b', 'batch-d', 'batch-c']);
			await page.getByLabel('Filter queue', { exact: true }).fill('Batch B');
			const remove = actions(page).getByRole('button', { name: 'Remove selected from queue', exact: true });
			await remove.focus();
			await page.keyboard.press('Enter');
			await expect(page.getByTestId('queue-announcement')).toHaveText('Removed 2 tracks from the queue.');
			await expect(page.getByTestId('queue-filter-empty')).toBeFocused();
			await page.getByRole('button', { name: 'Clear queue filter', exact: true }).click();
			await expect(page.getByTestId('queue-item')).toHaveCount(2);
			await expect(pick(page, 'a')).not.toBeChecked();
			await expect(pick(page, 'c')).not.toBeChecked();
			expect(await original.evaluate((el) => el === document.querySelector('[data-testid="audio-player"]'))).toBe(true);
			await expect(audio).toHaveAttribute('src', ITEMS[0].streamUrl);
			await expect(audio).toHaveJSProperty('paused', !playing);
			if (playing) await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBeGreaterThan(before);
			else expect(await audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBe(before);
			expect(await audio.evaluate((el) => (el as HTMLAudioElement & { batchEvents: string[] }).batchEvents)).toEqual([]);
			expect(requests.length).toBeGreaterThan(0);
			expect(requests.every((url) => url.endsWith('/batch-a'))).toBe(true);
			expect(errors).toEqual([]);
		});
	}
}

test('native checkbox ranges, scoped Select All, Done and repeated keyboard removal retain usable focus', async ({ page }) => {
	await setup(page);
	await pick(page, 'b').click();
	await pick(page, 'd').click({ modifiers: ['Shift'] });
	for (const id of ['b', 'c', 'd']) await expect(pick(page, id)).toBeChecked();
	await expect(pick(page, 'a')).not.toBeChecked();
	await actions(page).getByRole('button', { name: 'Done', exact: true }).focus();
	await page.keyboard.press('Enter');
	await expect(pick(page, 'b')).toBeFocused();
	await expect(actions(page)).toHaveCount(0);
	await page.keyboard.press('Space');
	await expect(pick(page, 'b')).toBeChecked();
	await page.keyboard.press('ControlOrMeta+a');
	for (const id of ['a', 'b', 'c', 'd']) await expect(pick(page, id)).toBeChecked();
	await actions(page).getByRole('button', { name: 'Remove selected from queue', exact: true }).focus();
	await page.keyboard.press('Enter');
	await expect(page.getByTestId('queue-empty-focus')).toBeFocused();
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('queue-announcement')).toHaveText('Removed 4 tracks from the queue.');
	await expect(actions(page)).toHaveCount(0);
});

test('bulk removal keeps the next live checkbox focused and selection survives actual panel unmount/remount', async ({ page }) => {
	await setup(page);
	await pick(page, 'b').click();
	await pick(page, 'c').click();
	await page.getByRole('button', { name: 'Close queue', exact: true }).click();
	await expect(page.getByTestId('queue-panel')).toHaveCount(0);
	await openQueue(page);
	await expect(pick(page, 'b')).toBeChecked();
	await expect(pick(page, 'c')).toBeChecked();
	await actions(page).getByRole('button', { name: 'Remove selected from queue', exact: true }).focus();
	await page.keyboard.press('Enter');
	await expect(pick(page, 'd')).toBeFocused();
	await expect(actions(page)).toContainText('0 selected');
	await page.keyboard.press('Space');
	await expect(pick(page, 'd')).toBeChecked();
	await expect(page.getByTestId('now-playing')).toContainText('Batch A');
});

for (const width of [320, 390, 1280]) {
	for (const appearance of ['light', 'dark']) {
		test(`populated selection controls reflow with visible keyboard focus and axe at ${width}px in ${appearance}`, async ({ page }) => {
			await page.setViewportSize({ width, height: 900 });
			await setup(page);
			await page.getByLabel('Appearance', { exact: true }).selectOption(appearance);
			await pick(page, 'b').click();
			await actions(page).getByRole('button', { name: 'Remove selected from queue', exact: true }).focus();
			await page.keyboard.press('Tab');
			const done = actions(page).getByRole('button', { name: 'Done', exact: true });
			await expect(done).toBeFocused();
			const ring = await done.evaluate((el) => {
				const style = getComputedStyle(el);
				return { visible: el.matches(':focus-visible'), width: parseFloat(style.outlineWidth),
					style: style.outlineStyle, color: style.outlineColor };
			});
			expect(ring.visible).toBe(true);
			expect(ring.width).toBeGreaterThanOrEqual(2);
			expect(ring.style).toBe('solid');
			expect(ring.color).not.toMatch(/^(transparent|rgba\([^)]*,\s*0\))$/);
			expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
			const result = await new AxeBuilder({ page }).withTags(['wcag2a', 'wcag2aa', 'wcag21aa']).analyze();
			expect(result.violations).toEqual([]);
			await test.info().attach('queue-selection', { body: await page.screenshot({ fullPage: true }), contentType: 'image/png' });
		});
	}
}
