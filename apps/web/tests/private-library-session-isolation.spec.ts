import { fileURLToPath } from 'node:url';
import { expect, test, type BrowserContext, type Page, type Request } from '@playwright/test';
import { preview } from 'vite';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';

const { library, player } = PRIVATE_MUSIC;
const sessionCookie = 'private_music_fixture_session';
const bindingHeader = 'x-nntune-session-binding';
const durationMs = 14_000;
const logoutPath = '/v1/session/logout';

type Invitee = {
	item: { id: string; title: string; duration_ms: number };
	binding: string;
	playId: string;
	audio: Buffer;
	mediaPath?: string;
};

async function wireInviteeContext(
	context: BrowserContext,
	origin: string,
	sessions: Map<string, Invitee>,
	cookie: string
) {
	await context.addCookies([{ name: sessionCookie, value: cookie, url: origin, httpOnly: true }]);
	const sessionToken = async (request: Request) =>
		(await request.headerValue('cookie'))?.split('; ')
			.find((part) => part.startsWith(`${sessionCookie}=`))?.slice(sessionCookie.length + 1) ?? '';
	const inviteeFor = async (request: Request) => sessions.get(await sessionToken(request));

	await context.route('**/*', (route) =>
		new URL(route.request().url()).origin === origin ? route.continue() : route.abort());
	await context.route(`${origin}/v1/me/subscription`, async (route) =>
		(await inviteeFor(route.request()))
			? route.fulfill({ json: { state: 'active' } })
			: route.fulfill({ status: 401 }));
	await context.route(`${origin}/v1/me/likes/session`, async (route) => {
		const invitee = await inviteeFor(route.request());
		return invitee ? route.fulfill({ json: { session_binding: invitee.binding } }) : route.fulfill({ status: 401 });
	});
	await context.route(`${origin}${library.api}`, async (route) => {
		const invitee = await inviteeFor(route.request());
		return invitee
			? route.fulfill({ headers: { [bindingHeader]: invitee.binding }, json: [invitee.item] })
			: route.fulfill({ status: 401 });
	});
	await context.route(`${origin}${player.plays}`, async (route) => {
		const request = route.request();
		const invitee = await inviteeFor(request);
		if (!invitee) return route.fulfill({ status: 401 });
		if (request.method() !== 'POST' || await request.headerValue(bindingHeader) !== invitee.binding ||
			await request.headerValue('x-nntune-browser-playback') !== '1') return route.fulfill({ status: 400 });
		const { track_id: trackId, session_id: playbackSessionId } = request.postDataJSON();
		if (trackId !== invitee.item.id || typeof playbackSessionId !== 'string' ||
			!/^[0-9a-f-]{36}$/.test(playbackSessionId)) return route.fulfill({ status: 400 });
		const { playId, binding } = invitee;
		invitee.mediaPath = `${player.plays}/media/${trackId}/${playbackSessionId}/${playId}`;
		return route.fulfill({ json: { trackId, playbackSessionId, sessionBinding: binding, playId, durationMs } });
	});
	await context.route(`${origin}${player.plays}/media/**`, async (route) => {
		const request = route.request();
		const invitee = await inviteeFor(request);
		if (!invitee?.mediaPath || request.url() !== `${origin}${invitee.mediaPath}`) return route.fulfill({ status: 404 });
		const { audio } = invitee;
		const range = /^bytes=(\d+)-(\d*)$/.exec(request.headers()['range'] ?? '');
		const start = range ? Number(range[1]) : 0;
		const end = range?.[2] ? Math.min(Number(range[2]), audio.length - 1) : audio.length - 1;
		if (start > end) return route.fulfill({ status: 416, headers: { 'content-range': `bytes */${audio.length}` } });
		return route.fulfill({
			status: range ? 206 : 200,
			contentType: 'audio/flac',
			body: audio.subarray(start, end + 1),
			headers: { 'accept-ranges': 'bytes', ...(range && { 'content-range': `bytes ${start}-${end}/${audio.length}` }) }
		});
	});
	await context.route(`${origin}${logoutPath}`, async (route) => {
		const request = route.request();
		if (request.method() !== 'POST' || await request.headerValue('origin') !== origin) return route.fulfill({ status: 403 });
		if (!sessions.delete(await sessionToken(request))) return route.fulfill({ status: 401 });
		return route.fulfill({
			headers: { 'set-cookie': `${sessionCookie}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0` },
			json: { ok: true }
		});
	});
}

async function expectOwnLibrary(page: Page, item: Invitee['item']) {
	const rows = page.getByTestId(library.item);
	await expect.poll(() => rows.evaluateAll((items, id) => items.map((row) => row.getAttribute(id)), library.id)).toEqual([item.id]);
}

function toggle(page: Page, name: 'Play' | 'Pause') {
	return page.getByRole('region', { name: 'Player', exact: true }).getByRole('button', { name, exact: true });
}

function playbackState(page: Page) {
	return page.getByTestId(player.audio).evaluate((audio: HTMLAudioElement) => ({
		paused: audio.paused,
		time: audio.currentTime,
		src: audio.getAttribute('src')
	}));
}

async function startOwnPlayback(page: Page, invitee: Invitee) {
	await expectOwnLibrary(page, invitee.item);
	await page.getByTestId(library.item).locator(library.select).click();
	await toggle(page, 'Play').click();
	await expect.poll(async () => {
		const { paused, time } = await playbackState(page);
		return !paused && time > 0.2;
	}).toBe(true);
	await expect(page.locator(player.currentQueueItem)).toHaveAttribute(player.queueId, invitee.item.id);
	expect((await playbackState(page)).src).toBe(invitee.mediaPath);
}

test.use({ serviceWorkers: 'block' });

test('private Library and playback stay isolated when another invitee signs out', async ({ browser, context, page }) => {
	const previous = process.env['PUBLIC_BROWSER_PLAYBACK'];
	process.env['PUBLIC_BROWSER_PLAYBACK'] = '1';
	const server = await preview({
		root: fileURLToPath(new URL('../', import.meta.url)), mode: 'browser-fixtures',
		preview: { host: '127.0.0.1', port: 0 }
	}).finally(() => {
		if (previous === undefined) delete process.env['PUBLIC_BROWSER_PLAYBACK'];
		else process.env['PUBLIC_BROWSER_PLAYBACK'] = previous;
	});
	try {
		const address = server.httpServer.address();
		if (!address || typeof address === 'string') throw new Error('Missing private fixture TCP address');
		const origin = `http://127.0.0.1:${address.port}`;
		const [first, second] = privateStagingAudio().map(({ body }, index): Invitee => ({
			item: { id: `00000000-0000-4000-8000-00000000000${index + 1}`, title: `Private tone ${index + 1}`, duration_ms: durationMs },
			binding: (index === 0 ? 'A' : 'B').repeat(43),
			playId: `00000000-0000-4000-8000-00000000001${index + 1}`,
			audio: body
		}));
		const sessions = new Map([['invitee-a', first], ['invitee-b', second]]);
		const otherContext = await browser.newContext({ serviceWorkers: 'block' });
		try {
			const other = await otherContext.newPage();
			await Promise.all([
				wireInviteeContext(context, origin, sessions, 'invitee-a'),
				wireInviteeContext(otherContext, origin, sessions, 'invitee-b')
			]);
			await Promise.all([page.goto(`${origin}${library.path}`), other.goto(`${origin}${library.path}`)]);
			await Promise.all([startOwnPlayback(page, first), startOwnPlayback(other, second)]);
			await toggle(other, 'Pause').click();
			await expect.poll(async () => (await playbackState(other)).paused).toBe(true);
			const before = await playbackState(other);
			expect(before.time).toBeGreaterThan(0.2);

			// Use ordinary sign-out in a second tab of the first invitee's browser.
			const security = await context.newPage();
			await security.goto(`${origin}/account/security`);
			const logout = security.waitForResponse(`${origin}${logoutPath}`);
			await security.getByRole('button', { name: 'Sign out', exact: true }).click();
			expect((await logout).status()).toBe(200);
			await security.waitForURL(`${origin}/login`);
			expect((await context.cookies(origin)).some(({ name }) => name === sessionCookie)).toBe(false);
			await expect(page.getByTestId(library.item)).toHaveCount(0);
			await expect(page.getByTestId(player.queueItem)).toHaveCount(0);
			await expect(page.getByTestId(player.audio)).toHaveCount(0);

			const refreshed = other.waitForResponse(`${origin}${library.api}`);
			await other.getByRole('button', { name: library.refresh, exact: true }).click();
			const response = await refreshed;
			expect(response.status()).toBe(200);
			expect(await response.headerValue(bindingHeader)).toBe(second.binding);
			await expectOwnLibrary(other, second.item);
			await expect(other.locator(player.currentQueueItem)).toHaveAttribute(player.queueId, second.item.id);
			expect(await playbackState(other)).toEqual(before);
			await toggle(other, 'Play').click();
			await expect.poll(async () => {
				const { paused, time, src } = await playbackState(other);
				return !paused && time > before.time && src === before.src;
			}).toBe(true);
		} finally {
			await otherContext.close();
		}
	} finally {
		await server.close();
	}
});
