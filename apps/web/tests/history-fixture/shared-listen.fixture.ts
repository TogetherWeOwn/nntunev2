import { expect, test, type Page } from '@playwright/test';
import { fileURLToPath } from 'node:url';
import { createServer, type ViteDevServer } from 'vite';
import { syntheticBody } from '../../src/lib/player/demoMedia.js';
import type {} from '../fixtures/shared-listen/mount.js';

const ids = ['A', 'Q', 'g'].map((last) => 'A'.repeat(21) + last);
const row = (n: number, event: string) => ({
	playback_id: `00000000-0000-0000-0000-00000000000${n}`, track_id: 'fixture-track', event
});
const journal = (page: Page) => page.evaluate(() => window.sharedListenFixture.read());
let server: ViteDevServer;
let url: string;

test.beforeAll(async () => {
	server = await createServer({
		configFile: false, publicDir: false,
		root: fileURLToPath(new URL('../fixtures/shared-listen/', import.meta.url)),
		server: { host: '127.0.0.1', port: 0, hmr: false, fs: { allow: [fileURLToPath(new URL('../../', import.meta.url))] } }
	});
	await server.listen();
	const address = server.httpServer?.address();
	if (!address || typeof address === 'string') throw new Error('no fixture TCP address');
	url = `http://127.0.0.1:${address.port}/library`;
});
test.afterAll(async () => { await server?.close(); });
test.beforeEach(async ({ page }) => {
	await page.goto(url);
	await expect(page.getByRole('heading', { name: 'Fixture Library' })).toBeVisible();
});

test('signed-in route replacement and pause/resume keep the same listen until completion', async ({ page }) => {
	await page.evaluate((id) => {
		const f = window.sharedListenFixture;
		f.issue(id); f.media('playing'); f.media('pause');
	}, ids[0]);
	const audio = await page.locator('audio').elementHandle();
	await page.getByRole('link', { name: 'Fixture Home' }).click();
	await expect(page).toHaveURL(/\/home$/);
	await expect(page.getByRole('heading', { name: 'Fixture Library' })).toHaveCount(0);
	expect(await audio?.evaluate((node) => node === document.querySelector('audio'))).toBe(true);
	await page.evaluate(() => window.sharedListenFixture.media('playing'));
	expect(await journal(page)).toEqual([row(0, 'start')]);
	await page.evaluate((next) => {
		const f = window.sharedListenFixture;
		f.media('ended'); f.media('ended'); f.abandon();
		f.issue(next); f.media('playing');
		// Source teardown clears audio.ended before the ended event arrives.
		f.setEnded(true); f.media('timeupdate'); f.setEnded(false);
		f.media('emptied');
	}, ids[1]);
	const completed = [row(0, 'start'), row(0, 'complete'), row(1, 'start'), row(1, 'complete')];
	expect(await journal(page)).toEqual(completed);
	await page.evaluate(() => window.sharedListenFixture.detach());
	expect(await journal(page)).toEqual(completed);
});

test('failed load is silent, native pause keeps the listen, and media error ends it once', async ({ page }) => {
	await page.route(new URL('/fixture-audio.wav', url).href, (route) => route.fulfill({
		contentType: 'audio/wav', body: Buffer.from(syntheticBody())
	}));
	const checkpoints = await page.evaluate(async ([first, next]) => {
		const f = window.sharedListenFixture;
		const audio = document.querySelector('audio')!;
		audio.muted = true;
		f.media('playing'); // No issued identity yet.
		f.issue(first);

		const loadError = new Promise<Event>((resolve) => audio.addEventListener('error', resolve, { once: true }));
		audio.src = 'data:audio/wav;base64,AAAA';
		const failedLoad = (await loadError).isTrusted && audio.error?.code === MediaError.MEDIA_ERR_SRC_NOT_SUPPORTED;
		const beforeStart = f.read();

		audio.src = '/fixture-audio.wav';
		await audio.play();
		const pause = new Promise<Event>((resolve) => audio.addEventListener('pause', resolve, { once: true }));
		audio.pause();
		const paused = (await pause).isTrusted && audio.paused;
		const afterPause = f.read();
		await audio.play();
		const afterResume = f.read();

		// Deliver an active playback error in isolation before any source teardown.
		f.media('error');
		const afterError = f.read();
		f.media('playing'); f.media('error'); f.media('emptied'); f.media('ended');
		const afterRepeats = f.read();
		f.issue(next); f.media('playing'); f.abandon(); f.abandon();
		const afterAbandon = f.read();
		f.detach();
		return { failedLoad, beforeStart, paused, afterPause, afterResume, afterError, afterRepeats, afterAbandon };
	}, ids);
	const skipped = [row(0, 'start'), row(0, 'skip'), row(1, 'start'), row(1, 'skip')];
	expect(checkpoints).toEqual({
		failedLoad: true,
		beforeStart: [],
		paused: true,
		afterPause: [row(0, 'start')],
		afterResume: [row(0, 'start')],
		afterError: skipped.slice(0, 2),
		afterRepeats: skipped.slice(0, 2),
		afterAbandon: skipped
	});
	expect(await journal(page)).toEqual(skipped);
});

test('persisted pagehide ends the listen and restoration needs fresh identity and playing', async ({ page }) => {
	await page.evaluate(([first, next]) => {
		const f = window.sharedListenFixture;
		f.issue(first); f.media('playing'); f.transition('pagehide'); f.transition('pageshow');
		f.media('playing'); f.transition('pagehide'); f.issue(next); f.media('playing'); f.transition('pageshow');
	}, ids);
	expect(await journal(page)).toEqual([row(0, 'start'), row(0, 'skip')]);
	await page.evaluate(() => window.sharedListenFixture.media('playing'));
	await page.reload();
	await expect(page.getByRole('heading', { name: 'Fixture Library' })).toBeVisible();
	expect(await journal(page)).toEqual([row(0, 'start'), row(0, 'skip'), row(1, 'start'), row(1, 'skip')]);
});

for (const source of ['local', 'storage'] as const) {
	test(`${source} auth reset cancels in-flight work and rejects a later grant`, async ({ page }) => {
		await page.evaluate((id) => {
			const f = window.sharedListenFixture;
			f.issue(id); f.hold(); f.media('playing');
		}, ids[0]);
		expect(await page.evaluate(() => window.sharedListenFixture.pending())).toBe('start');
		await page.evaluate(({ source, next }) => {
			const f = window.sharedListenFixture;
			f.reset(source); f.release(); f.issue(next); f.media('playing'); f.media('ended');
		}, { source, next: ids[1] });
		expect(await journal(page)).toEqual([]);
	});
}
