import { expect, test, type Page, type Route } from '@playwright/test';
import type {} from '../fixtures/listen-event-browser.js';

const events = '/v1/plays/events';
const contextPath = events + '/context';
const original = 'A'.repeat(42) + 'E';
const replacement = 'B'.repeat(42) + 'E';
const cookieName = 'history_fixture_session';
const cookieFor = (account: string) => cookieName + '=' + account;
const grantId = 'QMMlwCzuSJ2otvvpdBVWAQ';
const start = {
	playback_id: '40c325c0-2cee-489d-a8b6-fbe974155601',
	track_id: 'synthetic-track',
	event: 'start',
	context: original
};

test.use({ serviceWorkers: 'block' });

async function open(page: Page, origin: string, onEvent: (route: Route) => Promise<void>) {
	let contextReads = 0;
	const signIn = (value: string) => page.context().addCookies([{ name: cookieName, value, url: origin, httpOnly: true }]);
	await page.route('**/*', (route) => {
		const url = new URL(route.request().url());
		return url.origin !== origin || url.pathname.startsWith('/v1/') ? route.abort() : route.continue();
	});
	await page.route(origin + contextPath, async (route) => {
		const request = route.request();
		expect(request.method()).toBe('POST');
		contextReads++;
		const cookie = await request.headerValue('cookie');
		return route.fulfill({ json: { context: cookie === cookieFor('first') ? original : replacement } });
	});
	await page.route(origin + events, async (route) => {
		expect(route.request().method()).toBe('POST');
		await onEvent(route);
	});
	await signIn('first');
	await page.goto('/demo/player?fixture=listen-events');
	await expect.poll(() => page.evaluate(() => Boolean(window.__nntuneListenEventFixture))).toBe(true);
	return { contextReads: () => contextReads, signIn };
}

test('the browser ignores a private grant and posts a matching completion for an eligible grant', async ({ page, baseURL }) => {
	const bodies: unknown[] = [];
	const acknowledge = Promise.withResolvers<void>();
	const { contextReads } = await open(page, baseURL!, async (route) => {
		bodies.push(route.request().postDataJSON());
		if (bodies.length === 1) await acknowledge.promise;
		await route.fulfill({ json: { ok: true } });
	});
	try {
		const privateOpened = await page.evaluate(async (id) => {
			const fixture = window.__nntuneListenEventFixture!;
			const opened = await fixture.open('private_browser_' + id);
			fixture.playing();
			fixture.ended();
			return opened;
		}, grantId);
		expect(privateOpened).toBe(true);
		expect(await page.evaluate((id) => window.__nntuneListenEventFixture!.open(id), grantId)).toBe(true);
		await page.evaluate(() => window.__nntuneListenEventFixture!.playing());
		await expect.poll(() => bodies).toEqual([start]);
		await page.evaluate(() => window.__nntuneListenEventFixture!.ended());
	} finally {
		acknowledge.resolve();
	}
	await expect.poll(() => bodies).toEqual([start, { ...start, event: 'complete' }]);
	await page.evaluate(() => window.__nntuneListenEventFixture!.close());
	expect(contextReads()).toBe(1);
});

test('a retry with a changed browser cookie keeps the original listen context', async ({ page, baseURL }) => {
	const calls: { cookie: string | null; body: unknown }[] = [];
	const flip = Promise.withResolvers<void>();
	const { contextReads, signIn } = await open(page, baseURL!, async (route) => {
		const request = route.request();
		const cookie = await request.headerValue('cookie');
		const body = request.postDataJSON();
		calls.push({ cookie, body });
		if (calls.length === 1) {
			await flip.promise;
			await route.fulfill({ status: 503, json: {} });
		} else {
			const active = cookie === cookieFor('second') ? replacement : original;
			await route.fulfill({ status: body.context === active ? 200 : 401, json: { ok: true } });
		}
	});
	let refused: ReturnType<Page['waitForResponse']>;
	try {
		expect(await page.evaluate((id) => window.__nntuneListenEventFixture!.open(id), grantId)).toBe(true);
		await page.evaluate(() => window.__nntuneListenEventFixture!.playing());
		await expect.poll(() => calls).toEqual([{ cookie: cookieFor('first'), body: start }]);
		await page.evaluate(() => window.__nntuneListenEventFixture!.ended());
		await signIn('second');
		refused = page.waitForResponse((response) => response.url() === baseURL + events && response.status() === 401);
	} finally {
		flip.resolve();
	}
	await refused;
	await page.evaluate(() => window.__nntuneListenEventFixture!.close());
	expect(calls).toEqual([
		{ cookie: cookieFor('first'), body: start },
		{ cookie: cookieFor('second'), body: start }
	]);
	expect(contextReads()).toBe(1);
});
