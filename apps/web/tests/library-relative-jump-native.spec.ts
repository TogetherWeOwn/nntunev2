import { expect, test, type Page } from '@playwright/test';
import { createServer, request, type Server } from 'node:http';
import { lookup } from 'node:dns';
import type { AddressInfo } from 'node:net';

// Real Chromium media: no duration/currentTime/paused overrides. Range and
// progressive controls are local generated PCM, never a provider request.
function wave(seconds: number) {
	const bytes = seconds * 8000 * 2;
	const data = Buffer.alloc(44 + bytes);
	data.write('RIFF'); data.writeUInt32LE(36 + bytes, 4); data.write('WAVEfmt ', 8);
	data.writeUInt32LE(16, 16); data.writeUInt16LE(1, 20); data.writeUInt16LE(1, 22);
	data.writeUInt32LE(8000, 24); data.writeUInt32LE(16000, 28);
	data.writeUInt16LE(2, 32); data.writeUInt16LE(16, 34);
	data.write('data', 36); data.writeUInt32LE(bytes, 40);
	return data;
}
let server: Server;
let origin: string;
test.beforeAll(async ({}, info) => {
	const short = wave(20), long = wave(120);
	const upstream = info.project.use.baseURL!;
	server = createServer((req, res) => {
		if (!req.url?.startsWith('/native-audio/')) {
			const proxy = request(upstream, {
				path: req.url ?? '/',
				// Containers can have IPv6 loopback without a global IPv6 address.
				// Node's implicit ADDRCONFIG drops ::1 there even when Vite uses it.
				autoSelectFamily: true,
				lookup: (hostname, options, callback) => lookup(hostname, { ...options, hints: 0 }, callback)
			}, (response) => {
				res.writeHead(response.statusCode ?? 500, response.headers);
				response.pipe(res);
			});
			proxy.on('error', () => { res.writeHead(502); res.end(); });
			req.pipe(proxy);
			return;
		}
		res.setHeader('Access-Control-Allow-Origin', '*');
		res.setHeader('Content-Type', 'audio/wav');
		const data = req.url?.startsWith('/native-audio/progressive') ? long : short;
		if (req.url?.startsWith('/native-audio/progressive')) {
			res.writeHead(200, { 'Content-Length': data.length, 'Accept-Ranges': 'none' });
			// Keep the connection open after thirty seconds of a 120-second file.
			// Native duration is known while seeking beyond zero is unavailable.
			res.write(data.subarray(0, 44 + 30 * 16000));
			return;
		}
		const range = /^bytes=(\d+)-(\d*)$/.exec(req.headers.range ?? '');
		const start = range ? Number(range[1]) : 0;
		const end = range?.[2] ? Math.min(Number(range[2]), data.length - 1) : data.length - 1;
		res.setHeader('Accept-Ranges', 'bytes');
		if (range) res.setHeader('Content-Range', `bytes ${start}-${end}/${data.length}`);
		res.writeHead(range ? 206 : 200, { 'Content-Length': end - start + 1 });
		res.end(data.subarray(start, end + 1));
	});
	await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
	origin = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
});
test.afterAll(async () => {
	server.closeAllConnections();
	await new Promise<void>((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
});

async function open(page: Page, progressive = false) {
	const items = [
		{ id: 'native-a', title: 'Native A', streamUrl: `/native-audio/${progressive ? 'progressive' : 'range'}/a.wav` },
		{ id: 'native-b', title: 'Native B', streamUrl: '/native-audio/range/b.wav' }
	];
	await page.route('**/api/library', (route) => route.fulfill({ json: items }));
	const response = await page.goto(`${origin}/library`);
	expect(response?.status(), 'native fixture preview proxy must reach the configured upstream').toBe(200);
	for (const item of items) {
		await page.getByTestId('library-item').filter({ has: page.getByRole('button', { name: item.title, exact: true }) })
			.getByRole('button', { name: item.title, exact: true }).click();
	}
	const audio = page.getByTestId('audio-player');
	await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.duration)).toBe(progressive ? 120 : 20);
	return audio;
}

for (const repeat of ['off', 'one', 'queue']) {
	test(`explicit Forward-to-end never invokes natural ${repeat} completion`, async ({ page }) => {
		const audio = await open(page);
		await page.getByRole('combobox', { name: 'Repeat' }).selectOption(repeat);
		await audio.evaluate(async (el: HTMLAudioElement) => {
			el.volume = 0.35; el.playbackRate = 1.5; el.currentTime = 15;
			await el.play();
		});
		await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => !el.paused)).toBe(true);
		const original = await audio.getAttribute('src');
		await page.getByTestId('player-seek-forward').click();
		await expect(page.getByTestId('player-seek-feedback')).toHaveText('Forward to the end, 0:20.');
		await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.ended)).toBe(true);
		// Ended has fired; allow its async tick/play path to run before inspecting identity.
		await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));
		await expect(page.getByTestId('now-playing')).toContainText('Native A');
		await expect(audio).toHaveAttribute('src', original!);
		expect(await audio.evaluate((el: HTMLAudioElement) => ({ time: el.currentTime, volume: el.volume, rate: el.playbackRate })))
			.toEqual({ time: 20, volume: 0.35, rate: 1.5 });
	});
}

test('progressive non-range media never announces an unperformed jump', async ({ page }) => {
	const audio = await open(page, true);
	await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.seekable.length)).toBeGreaterThan(0);
	expect(await audio.evaluate((el: HTMLAudioElement) => el.seekable.end(0))).toBe(0);
	await page.getByTestId('player-seek-forward').click();
	await expect(page.getByTestId('player-seek-feedback')).toHaveText(/^(Cannot jump|The player did not accept)/);
	expect(await audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBe(0);
	await expect(page.getByTestId('now-playing')).toContainText('Native A');
});

test('range-capable native control settles at the requested position', async ({ page }) => {
	const audio = await open(page);
	await page.getByTestId('player-seek-forward').click();
	await expect(page.getByTestId('player-seek-feedback')).toHaveText('Forward 10 seconds, now at 0:10.');
	expect(await audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBeCloseTo(10, 2);
});

test('native replay after an explicit end retires the jump fence', async ({ page }) => {
	const audio = await open(page);
	await audio.evaluate(async (el: HTMLAudioElement) => { el.currentTime = 15; await el.play(); });
	await page.getByTestId('player-seek-forward').click();
	await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.ended)).toBe(true);
	await expect(page.getByTestId('now-playing')).toContainText('Native A');
	await audio.evaluate(async (el: HTMLAudioElement) => { el.currentTime = 19.5; await el.play(); });
	await expect(page.getByTestId('now-playing')).toContainText('Native B');
});

test('natural completion still advances after a real playback end', async ({ page }) => {
	const audio = await open(page);
	await audio.evaluate(async (el: HTMLAudioElement) => { el.currentTime = 19.5; await el.play(); });
	await expect(page.getByTestId('now-playing')).toContainText('Native B');
});
