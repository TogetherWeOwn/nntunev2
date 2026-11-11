import { Buffer } from 'node:buffer';
import AxeBuilder from '@axe-core/playwright';
import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Builder-owned browser spec for the layout mini-player (mobile breakpoint is
 * max-width 799px). Real Chromium native decode of a locally served seekable
 * PCM WAV; no play/pause/metadata is fabricated. The interim full
 * PlayerTransport stays reachable and visible on mobile, so tests use its
 * player-favorite / player-seek / player-play-pause as the second surface and
 * never expect it to be hidden.
 */

interface Item { id: string; title: string; artist: string; streamUrl?: string }

const A: Item = { id: 'mini-a', title: 'Current track', artist: 'Test artist', streamUrl: '/test-audio/mini-a' };
const B: Item = { id: 'mini-b', title: 'Second track', artist: 'Other artist', streamUrl: '/test-audio/mini-b' };
const NO_STREAM: Item = { id: 'mini-meta', title: 'Metadata only', artist: 'Quiet artist' };
const LONG: Item = {
	id: 'mini-long',
	title: 'Endless <i>literal</i> & unbroken title that keeps going well past any narrow mobile bar width 0123456789',
	artist: 'An artist with a deliberately very long name & <b>markup</b> that also exceeds the bar',
	streamUrl: '/test-audio/mini-long'
};
const SECONDS = 60;

function wav(): Buffer {
	const bytes = SECONDS * 8000 * 2;
	const out = Buffer.alloc(44 + bytes);
	out.write('RIFF'); out.writeUInt32LE(36 + bytes, 4); out.write('WAVEfmt ', 8);
	out.writeUInt32LE(16, 16); out.writeUInt16LE(1, 20); out.writeUInt16LE(1, 22);
	out.writeUInt32LE(8000, 24); out.writeUInt32LE(16000, 28);
	out.writeUInt16LE(2, 32); out.writeUInt16LE(16, 34);
	out.write('data', 36); out.writeUInt32LE(bytes, 40);
	return out;
}

async function seed(page: Page, items: Item[]) {
	await page.route('**/api/library', (route) => route.fulfill({ json: items }));
	const body = wav();
	await page.route('**/test-audio/**', (route) => {
		// Honor actual media Range requests so Chromium can really seek.
		const range = /^bytes=(\d+)-(\d*)$/.exec(route.request().headers()['range'] ?? '');
		const start = range ? Number(range[1]) : 0;
		const end = range?.[2] ? Math.min(Number(range[2]), body.length - 1) : body.length - 1;
		if (start > end) return route.fulfill({ status: 416, headers: { 'Content-Range': `bytes */${body.length}` } });
		return route.fulfill({
			status: range ? 206 : 200,
			contentType: 'audio/wav',
			headers: { 'Accept-Ranges': 'bytes', ...(range ? { 'Content-Range': `bytes ${start}-${end}/${body.length}` } : {}) },
			body: body.subarray(start, end + 1)
		});
	});
	await page.goto('/library');
}

async function select(page: Page, item: Item) {
	const prefix = item.title.slice(0, 7).replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
	await page.getByTestId('library-item').getByRole('button', { name: new RegExp(`^${prefix}`) }).first().click();
}

async function selectPlayable(page: Page, item: Item) {
	await select(page, item);
	await expect(page.getByTestId('audio-player')).toHaveJSProperty('readyState', 4);
	await expect(page.getByTestId('player-seek')).toHaveAttribute('max', String(SECONDS));
	await expect.poll(() => page.getByTestId('audio-player').evaluate((el: HTMLAudioElement) => el.seekable.length)).toBeGreaterThan(0);
}

async function clientLink(page: Page, path: string) {
	await page.evaluate((path) => {
		const link = document.createElement('a'); link.href = path;
		link.textContent = 'Navigate to test route'; document.body.append(link); link.click();
	}, path);
}

const time = (audio: Locator) => audio.evaluate((el: HTMLAudioElement) => el.currentTime);
const barWidth = (page: Page) =>
	page.getByTestId('mini-player-progress').evaluate((el) => {
		const parent = el.parentElement!.getBoundingClientRect().width;
		return parent ? (el.getBoundingClientRect().width / parent) * 100 : NaN;
	});
const frames = (page: Page) =>
	page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));

async function box(locator: Locator) {
	const b = await locator.boundingBox();
	if (!b) throw new Error('element has no box');
	return b;
}

test.describe('mini-player live controls', () => {
	test.beforeEach(async ({ page }) => {
		await page.setViewportSize({ width: 390, height: 844 });
	});

	test('Play/Pause drives the one decoder with real progress and never navigates', async ({ page }) => {
		await seed(page, [A]);
		await selectPlayable(page, A);
		const audio = page.getByTestId('audio-player');
		const original = (await audio.elementHandle())!;
		const mini = page.getByTestId('mini-player');
		const play = page.getByTestId('player-play-pause');
		await expect(play).toBeEnabled();
		await expect(play).toHaveAccessibleName('Play');
		await expect(audio).toHaveJSProperty('paused', true);
		expect(await barWidth(page)).toBe(0);
		await audio.evaluate((el) => {
			const events: string[] = [];
			for (const name of ['loadstart', 'emptied']) el.addEventListener(name, () => events.push(name));
			Object.assign(el, { miniEvents: events });
		});

		await play.click();
		await expect(audio).toHaveJSProperty('paused', false);
		await expect(play).toHaveAccessibleName('Pause');
		await expect(page.getByTestId('player-play-pause')).toHaveAccessibleName('Pause');
		await expect.poll(() => time(audio)).toBeGreaterThan(0.3);
		await expect.poll(() => barWidth(page)).toBeGreaterThan(0);
		await expect(mini).toBeVisible();
		await expect(page).toHaveURL(/\/library$/);

		await play.click();
		await expect(audio).toHaveJSProperty('paused', true);
		await expect(play).toHaveAccessibleName('Play');
		await expect(page.getByTestId('player-play-pause')).toHaveAccessibleName('Play');
		const frozen = await time(audio);
		const frozenWidth = await barWidth(page);
		await frames(page); await frames(page);
		expect(await time(audio)).toBe(frozen);
		expect(await barWidth(page)).toBe(frozenWidth);

		expect(await original.evaluate((el) => el === document.querySelector('[data-testid="audio-player"]'))).toBe(true);
		await expect(audio).toHaveCount(1);
		await expect(audio).toHaveAttribute('src', A.streamUrl!);
		await expect(page).toHaveURL(/\/library$/);
		expect(await audio.evaluate((el) => (el as HTMLAudioElement & { miniEvents: string[] }).miniEvents)).toEqual([]);
	});

	test('the single favorite surface toggles in both directions', async ({ page }) => {
		await seed(page, [A]);
		await selectPlayable(page, A);
		const heart = page.getByTestId('player-favorite');
		await expect(heart).toHaveCount(1);
		await expect(heart).toHaveAccessibleName('Save to Favorites');
		await expect(heart).toHaveAttribute('aria-pressed', 'false');
		await heart.click();
		await expect(heart).toHaveAttribute('aria-pressed', 'true');
		await expect(heart).toHaveAccessibleName('Remove from Favorites');
		await page.getByTestId('player-now-playing-toggle').click();
		await expect(page.getByTestId('now-playing-route')).toBeVisible();
		const expanded = page.getByTestId('now-playing-route').getByTestId('player-favorite');
		await expect(expanded).toHaveAttribute('aria-pressed', 'true');
		await expanded.click();
		await expect(expanded).toHaveAttribute('aria-pressed', 'false');
		await page.getByRole('button', { name: 'Close Now playing', exact: true }).click();
		await expect(page).toHaveURL(/\/library$/);
		await expect(heart).toHaveAttribute('aria-pressed', 'false');
	});

	for (const playing of [false, true]) {
		test(`Now playing round trip keeps the same decoder ${playing ? 'playing' : 'paused'} with a continuous clock`, async ({ page }) => {
			await seed(page, [A]);
			await selectPlayable(page, A);
			const audio = page.getByTestId('audio-player');
			const original = (await audio.elementHandle())!;
			await page.getByTestId('mini-player').getByRole('link').click();
			await expect(page).toHaveURL(/\/now-playing$/);
			await expect(page.getByTestId('now-playing-route')).toBeVisible();
			await expect(page.getByTestId('mini-player')).toHaveCount(0);
			await page.getByTestId('now-playing-route').getByTestId('player-seek').fill('20');
			await expect.poll(() => time(audio)).toBeGreaterThan(19.5);
			await expect(audio).toHaveJSProperty('seeking', false);
			if (playing) {
				await page.getByTestId('player-play-pause').click();
				await expect(audio).toHaveJSProperty('paused', false);
				// The property changes before the queued native play event.
				// Start the navigation probe only after real playback progresses.
				await expect.poll(() => time(audio)).toBeGreaterThan(20.2);
			}
			const before = await time(audio);
			await audio.evaluate((el) => {
				const events: string[] = [];
				for (const name of ['play', 'pause', 'loadstart', 'emptied', 'seeking']) el.addEventListener(name, () => events.push(name));
				Object.assign(el, { miniEvents: events });
			});
			await expect(audio).toHaveJSProperty('paused', !playing);
			if (playing) await expect.poll(() => time(audio)).toBeGreaterThan(before + 0.3);
			else expect(await time(audio)).toBe(before);

			await page.getByRole('button', { name: 'Close Now playing', exact: true }).click();
			await expect(page).toHaveURL(/\/library$/);
			const play = page.getByTestId('player-play-pause');
			await expect(play).toHaveAccessibleName(playing ? 'Pause' : 'Play');
			await expect(audio).toHaveJSProperty('paused', !playing);
			const mid = await time(audio);
			if (playing) await expect.poll(() => time(audio)).toBeGreaterThan(mid + 0.3);
			else expect(await time(audio)).toBe(before);
			expect(mid).toBeGreaterThanOrEqual(before);
			expect(await original.evaluate((el) => el === document.querySelector('[data-testid="audio-player"]'))).toBe(true);
			await expect(audio).toHaveAttribute('src', A.streamUrl!);
			expect(await audio.evaluate((el) => (el as HTMLAudioElement & { miniEvents: string[] }).miniEvents)).toEqual([]);
		});
	}

	test('progress follows seeks, is not a slider, and ignores clicks', async ({ page }) => {
		await seed(page, [A]);
		await selectPlayable(page, A);
		const audio = page.getByTestId('audio-player');
		const mini = page.getByTestId('mini-player');
		// Seeking lives in the expanded route; the compact bar only reports it.
		await mini.getByRole('link').click();
		await expect(page.getByTestId('now-playing-route')).toBeVisible();
		await page.getByTestId('now-playing-route').getByTestId('player-seek').fill('30');
		await expect.poll(() => time(audio)).toBeGreaterThan(29.5);
		await page.getByRole('button', { name: 'Close Now playing', exact: true }).click();
		await expect(page).toHaveURL(/\/library$/);
		await expect.poll(() => barWidth(page)).toBeGreaterThan(47);
		expect(await barWidth(page)).toBeLessThan(53);
		await expect(page.getByTestId('mini-player-progress')).toHaveAttribute('style', /width:\s*[\d.]+%/);

		await expect(mini.getByRole('slider')).toHaveCount(0);
		await expect(mini.locator('[role="slider"], [role="progressbar"], input')).toHaveCount(0);
		const track = page.getByTestId('mini-player-progress').locator('..');
		await expect(track).toHaveAttribute('aria-hidden', 'true');
		expect((await box(track)).height).toBe(2);
		const at = await time(audio);
		const area = await box(track);
		await page.mouse.click(area.x + area.width * 0.1, area.y + area.height / 2);
		await frames(page);
		expect(await time(audio)).toBe(at);
		expect(await barWidth(page)).toBeGreaterThan(47);
		await expect(page).toHaveURL(/\/library$/);
	});

	test('switching the current track retargets the mini-player and resets progress', async ({ page }) => {
		await seed(page, [A, B]);
		await selectPlayable(page, A);
		const audio = page.getByTestId('audio-player');
		const link = page.getByTestId('mini-player').getByRole('link');
		await expect(link).toHaveAccessibleName(`Now playing: ${A.title} — ${A.artist}`);
		await link.click();
		await expect(page.getByTestId('now-playing-route')).toBeVisible();
		await page.getByTestId('now-playing-route').getByTestId('player-seek').fill('30');
		await expect.poll(() => time(audio)).toBeGreaterThan(29.5);
		await page.getByTestId('player-favorite').click();
		await expect(page.getByTestId('player-favorite')).toHaveAttribute('aria-pressed', 'true');
		await page.getByRole('button', { name: 'Close Now playing', exact: true }).click();
		await expect(page).toHaveURL(/\/library$/);
		await expect.poll(() => barWidth(page)).toBeGreaterThan(40);
		await select(page, B);
		// Selecting a Library row appends; Next explicitly changes current.
		await link.click();
		await expect(page.getByTestId('now-playing-route')).toBeVisible();
		await page.getByTestId('player-next').click();
		await expect(audio).toHaveAttribute('src', B.streamUrl!);
		await page.getByRole('button', { name: 'Close Now playing', exact: true }).click();
		await expect(page).toHaveURL(/\/library$/);
		await expect(link).toHaveAccessibleName(`Now playing: ${B.title} — ${B.artist}`);
		await expect.poll(() => barWidth(page)).toBeLessThan(5);
		await expect(page.getByTestId('player-favorite')).toHaveAttribute('aria-pressed', 'false');
	});

	test('empty library has no mini-player', async ({ page }) => {
		await seed(page, []);
		await expect(page.getByTestId('library-item')).toHaveCount(0);
		await expect(page.getByTestId('mini-player')).toHaveCount(0);
		await page.goto('/now-playing');
		await expect(page.getByTestId('mini-player')).toHaveCount(0);
	});

	test('metadata-only selection shows plain identity with Play disabled and nothing audible', async ({ page }) => {
		await seed(page, [NO_STREAM]);
		await select(page, NO_STREAM);
		const mini = page.getByTestId('mini-player');
		await expect(mini).toBeVisible();
		await expect(mini).toContainText(`${NO_STREAM.title} — ${NO_STREAM.artist}`);
		await expect(mini.getByRole('link')).toHaveCount(0);
		const play = page.getByTestId('player-play-pause');
		await expect(play).toBeDisabled();
		await expect(play).toHaveAccessibleName('Play');
		expect(await barWidth(page)).toBe(0);
		await expect(page.getByTestId('audio-player')).toHaveCount(0);
	});

	test('auth boundary away from Library drops the mini-player', async ({ page }) => {
		await seed(page, [A]);
		await selectPlayable(page, A);
		await expect(page.getByTestId('mini-player')).toBeVisible();
		const audio = page.getByTestId('audio-player');
		const original = (await audio.elementHandle())!;
		await page.getByTestId('player-play-pause').click();
		await expect(audio).toHaveJSProperty('paused', false);
		await expect.poll(() => time(audio)).toBeGreaterThan(0.3);
		await clientLink(page, '/home');
		await expect(page).toHaveURL(/\/home$/);
		await expect(page.getByTestId('mini-player')).toBeVisible();
		await expect(audio).toHaveJSProperty('paused', false);
		await page.evaluate(() => window.dispatchEvent(new StorageEvent('storage', { key: 'nntune.library.auth-transition.v1' })));
		await expect(page.getByTestId('mini-player')).toHaveCount(0);
		await expect(audio).toHaveCount(0);
		expect(await original.evaluate((el) => (el as HTMLAudioElement).paused)).toBe(true);
	});
});

const LONG_FILLERS: Item[] = Array.from({ length: 40 }, (_, i) => ({ id: `mini-fill-${i}`, title: `Filler ${i}`, artist: 'Filler artist' }));

for (const width of [320, 390, 799]) {
	for (const appearance of ['light', 'dark']) {
		test(`mini-player geometry, keyboard and accessibility at ${width}px in ${appearance}`, async ({ page }) => {
			await page.setViewportSize({ width, height: 640 });
			await seed(page, [LONG, ...LONG_FILLERS]);
			await page.getByLabel('Appearance', { exact: true }).selectOption(appearance);
			await selectPlayable(page, LONG);
			const mini = page.getByTestId('mini-player');
			const link = mini.getByRole('link');
			const heart = page.getByTestId('player-favorite');
			const restart = page.getByTestId('library-restart');
			const play = page.getByTestId('player-play-pause');
			const tabbar = page.getByTestId('shell-tabbar');
			await expect(mini).toBeVisible();

			// Literal, complete accessible identity; one line; nothing overflows.
			await expect(link).toHaveAccessibleName(`Now playing: ${LONG.title} — ${LONG.artist}`);
			const identity = mini.locator('.identity-link .identity');
			await expect(identity).toHaveText(`${LONG.title} — ${LONG.artist}`);
			await expect(mini.locator('b, i')).toHaveCount(0);
			const style = await identity.evaluate((el) => {
				const css = getComputedStyle(el);
				return { whiteSpace: css.whiteSpace, fontSize: parseFloat(css.fontSize), height: el.getBoundingClientRect().height };
			});
			expect(style.whiteSpace).toBe('nowrap');
			expect(style.height).toBeLessThan(style.fontSize * 2);
			expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);

			// Exact bar, art and control sizes, all inside the bar without overlap.
			const bar = await box(mini);
			expect(bar.height).toBe(56);
			expect(bar.x).toBeGreaterThanOrEqual(0);
			expect(bar.x + bar.width).toBeLessThanOrEqual(width + 0.5);
			const art = await box(mini.locator('.compact-art'));
			expect([art.width, art.height]).toEqual([40, 40]);
			const [l, h, p] = [await box(link), await box(heart), await box(play)];
			expect(l.height).toBeGreaterThanOrEqual(44);
			for (const control of [h, p]) {
				expect(control.width).toBeGreaterThanOrEqual(44);
				expect(control.height).toBeGreaterThanOrEqual(44);
			}
			for (const control of [l, h, p]) {
				expect(control.x).toBeGreaterThanOrEqual(bar.x - 0.5);
				expect(control.x + control.width).toBeLessThanOrEqual(bar.x + bar.width + 0.5);
				expect(control.y).toBeGreaterThanOrEqual(bar.y - 0.5);
				expect(control.y + control.height).toBeLessThanOrEqual(bar.y + bar.height + 0.5);
			}
			expect(l.x + l.width).toBeLessThanOrEqual(h.x + 0.5);
			expect(h.x + h.width).toBeLessThanOrEqual(p.x + 0.5);
			const track = page.getByTestId('mini-player-progress').locator('..');
			expect((await box(track)).height).toBe(2);
			await expect(track).toHaveAttribute('aria-hidden', 'true');

			// Sits directly on the tab bar (the bar's top border may add 1px) and
			// stays there while the page scrolls; the end of main stays reachable.
			const adjacent = async () => {
				const m = await box(mini);
				const t = await box(tabbar);
				expect(Math.abs(m.y + m.height - t.y)).toBeLessThanOrEqual(1);
				expect(Math.abs(t.y + t.height - 640)).toBeLessThanOrEqual(1);
			};
			await adjacent();
			expect(await page.evaluate(() => document.documentElement.scrollHeight > innerHeight + 200)).toBe(true);
			await page.evaluate(() => window.scrollTo(0, 250));
			await adjacent();
			await page.evaluate(() => window.scrollTo(0, document.documentElement.scrollHeight));
			await adjacent();
			const end = await page.evaluate(() => document.querySelector('main')!.getBoundingClientRect().bottom);
			expect(end).toBeLessThanOrEqual((await box(mini)).y + 1);
			await page.evaluate(() => window.scrollTo(0, 0));

			const result = await new AxeBuilder({ page }).withTags(['wcag2a', 'wcag2aa', 'wcag21aa']).analyze();
			expect(result.violations).toEqual([]);
			await page.screenshot({ path: `test-results/mini-player-${width}-${appearance}.png` });

			// Keyboard: visible ring on each stop, Enter/Space activation.
			await page.keyboard.press('Shift');
			await link.focus();
			for (const target of [link, heart, restart, play]) {
				await expect(target).toBeFocused();
				const ring = await target.evaluate((el) => {
					const css = getComputedStyle(el);
					return { focusVisible: el.matches(':focus-visible'), style: css.outlineStyle, width: parseFloat(css.outlineWidth) };
				});
				expect(ring.focusVisible).toBe(true);
				expect(ring.style).not.toBe('none');
				expect(ring.width).toBeGreaterThan(0);
				if (target !== play) await page.keyboard.press('Tab');
			}
			const audio = page.getByTestId('audio-player');
			await page.keyboard.press('Space');
			await expect(audio).toHaveJSProperty('paused', false);
			await expect(play).toHaveAccessibleName('Pause');
			await page.keyboard.press('Enter');
			await expect(audio).toHaveJSProperty('paused', true);
			await expect(play).toHaveAccessibleName('Play');
			await page.keyboard.press('Shift+Tab');
			await expect(restart).toBeFocused();
			await page.keyboard.press('Shift+Tab');
			await expect(heart).toBeFocused();
			await page.keyboard.press('Enter');
			await expect(heart).toHaveAttribute('aria-pressed', 'true');
			await page.keyboard.press('Space');
			await expect(heart).toHaveAttribute('aria-pressed', 'false');
			await page.keyboard.press('Shift+Tab');
			await expect(link).toBeFocused();
			await page.keyboard.press('Enter');
			await expect(page).toHaveURL(/\/now-playing$/);
			await expect(mini).toHaveCount(0);
		});
	}
}
