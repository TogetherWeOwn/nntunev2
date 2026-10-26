import { request } from 'node:http';
import { chromium } from '@playwright/test';
import { createProbeServer } from './server.mjs';

const server = await createProbeServer(new URL('./dist', import.meta.url));
await new Promise((r) => server.listen(43173, '127.0.0.1', r));

// Raw-path traversal self-check (fetch would normalize `..` away first).
const traversalStatus = await new Promise((resolveStatus) => {
	const probe = request(
		{ host: '127.0.0.1', port: 43173, path: '/../vite.config.ts' },
		(response) => {
			response.resume();
			resolveStatus(response.statusCode ?? 0);
		}
	);
	probe.on('error', () => resolveStatus(0));
	probe.end();
});

const channel = process.env.PROBE_CHANNEL;
let browser;
try {
	browser = await chromium.launch(channel ? { channel } : {});
	const page = await browser.newPage();
	await page.goto('http://127.0.0.1:43173/');
	const version = browser.version();
	await page.click('#open');
	await page.waitForFunction(() => window.__probe?.mountedText || window.__probe?.errorName, null, { timeout: 10000 });
	const probe = await page.evaluate(() => window.__probe);
	let afterClose = null;
	if (probe?.opened) {
		await page.evaluate(() => window.__pipWindow?.close());
		await page.waitForFunction(() => window.__probe?.pagehideFired, null, { timeout: 10000 });
		afterClose = await page.evaluate(() => window.__probe);
	}
	console.log(JSON.stringify({ channel: channel ?? 'default-headless-shell', version, traversalStatus, probe, afterClose }, null, 2));
	if (traversalStatus !== 404 || !probe?.isSecureContext || !probe?.hasApi || !probe?.opened ||
		probe?.errorName || probe?.mountedText !== 'Probe mounted' ||
		probe?.computedColor !== 'rgb(255, 0, 0)' || !(probe?.clonedRules > 0) || !afterClose?.pagehideFired) {
		process.exitCode = 1;
	}
} catch (error) {
	console.log(JSON.stringify({ channel: channel ?? 'default-headless-shell', launchError: String(error).split('\n').slice(0, 6) }, null, 2));
	process.exitCode = 1;
} finally {
	await browser?.close().catch(() => {});
	server.close();
}
