import { createServer, request } from 'node:http';
import { readFile } from 'node:fs/promises';
import { extname, join, resolve, sep } from 'node:path';
import { chromium } from '@playwright/test';

const distRoot = resolve(new URL('./dist', import.meta.url).pathname);
const types = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css' };
const server = createServer(async (req, res) => {
	const path = (req.url === '/' ? '/index.html' : req.url ?? '').split('?')[0];
	// Confine every request to the built probe output: a raw `../` path
	// must never read sources or anything else outside dist/.
	const filePath = resolve(join(distRoot, `.${path}`));
	if (filePath !== distRoot && !filePath.startsWith(distRoot + sep)) {
		res.writeHead(404).end();
		return;
	}
	try {
		const body = await readFile(filePath);
		res.writeHead(200, { 'content-type': types[extname(path)] ?? 'application/octet-stream' });
		res.end(body);
	} catch {
		res.writeHead(404).end();
	}
});
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
	await page.waitForTimeout(1500);
	const probe = await page.evaluate(() => window.__probe);
	let afterClose = null;
	if (probe?.opened) {
		await page.evaluate(() => window.__pipWindow?.close());
		await page.waitForTimeout(500);
		afterClose = await page.evaluate(() => window.__probe);
	}
	console.log(JSON.stringify({ channel: channel ?? 'default-headless-shell', version, traversalStatus, probe, afterClose }, null, 2));
} catch (error) {
	console.log(JSON.stringify({ channel: channel ?? 'default-headless-shell', launchError: String(error).split('\n').slice(0, 6) }, null, 2));
} finally {
	await browser?.close().catch(() => {});
	server.close();
}
