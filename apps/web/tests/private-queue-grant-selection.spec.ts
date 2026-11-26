import { expect, type Request } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';
import { fixtureCookie, hasFixtureSession, observeMediaSources, serveFlac, test } from './helpers/private-music-fixture.js';

test('the newest private queue choice wins while an earlier grant is pending', async ({ page, context, origin }) => {
	const { library, player } = PRIVATE_MUSIC;
	const track = (number: number, title: string) => ({
		id: `11000000-0000-4000-8000-00000000000${number}`, title, duration_ms: 14_000
	});
	const initial = track(1, 'Initially selected');
	const earlier = track(2, 'Earlier choice');
	const latest = track(3, 'Latest choice');
	const catalogue = [initial, earlier, latest];
	const listener = 'synthetic-listener';
	const binding = 'A'.repeat(43);
	const playsUrl = origin + player.plays;
	const mediaPrefix = `${playsUrl}/media/`;
	const earlierPlayId = 'private_browser_000000000000000000000A';
	const latestPlayId = 'private_browser_000000000000000000000B';
	const releaseEarlier = Promise.withResolvers<void>();
	const releaseLatest = Promise.withResolvers<void>();
	const grants: Request[] = [];
	const mediaRequests = new Set<string>();
	const [tone] = privateStagingAudio();
	let activeMedia: string | undefined;
	const bound = async (request: Request) => hasFixtureSession(await request.headerValue('cookie'), listener);

	await context.route('**/*', (route) => {
		const url = new URL(route.request().url());
		return url.origin !== origin || /^\/(?:api|v1)\//.test(url.pathname) ? route.abort() : route.continue();
	});
	await context.addCookies([{ name: fixtureCookie, value: listener, url: origin, httpOnly: true }]);
	await page.route(`${origin}/v1/me/likes/session`, async (route) => await bound(route.request())
		? route.fulfill({ json: { session_binding: binding } }) : route.fulfill({ status: 401 }));
	await page.route(origin + library.api, async (route) => await bound(route.request())
		? route.fulfill({ headers: { 'x-nntune-session-binding': binding }, json: catalogue }) : route.fulfill({ status: 401 }));
	page.on('request', (request) => {
		if (request.url() === playsUrl) grants.push(request);
		if (request.url().startsWith(mediaPrefix)) mediaRequests.add(request.url());
	});
	await page.route(playsUrl, async (route) => {
		const request = route.request();
		if (request.method() !== 'POST' || !await bound(request) ||
			await request.headerValue('x-nntune-browser-playback') !== '1' ||
			await request.headerValue('x-nntune-session-binding') !== binding) return route.fulfill({ status: 403 });
		const { track_id, session_id, play_id } = request.postDataJSON() ?? {};
		const target = catalogue.find(({ id }) => id === track_id);
		if (!target || target === initial ||
			typeof session_id !== 'string' || !/^[0-9a-f-]{36}$/.test(session_id) || play_id !== undefined) {
			return route.fulfill({ status: 400 });
		}
		const isEarlier = target === earlier;
		const playId = isEarlier ? earlierPlayId : latestPlayId;
		// The server replaces the lease before its response reaches the browser.
		activeMedia = `${mediaPrefix}${track_id}/${session_id}/${playId}`;
		await (isEarlier ? releaseEarlier.promise : releaseLatest.promise);
		return route.fulfill({ json: {
			trackId: track_id, playbackSessionId: session_id, sessionBinding: binding,
			playId, durationMs: target.duration_ms
		} });
	});
	await page.route(`${mediaPrefix}**`, async (route) => {
		const request = route.request();
		if (request.method() !== 'GET' || request.url() !== activeMedia || !await bound(request)) {
			return route.fulfill({ status: 404 });
		}
		return serveFlac(route, tone.body);
	});

	const queue = page.getByTestId(player.queueItem);
	const selected = page.locator(player.currentQueueItem);
	const audio = page.getByTestId(player.audio);
	const queueIds = () => queue.evaluateAll((rows, attr) => rows.map((row) => row.getAttribute(attr)), player.queueId);
	const choose = async (title: string) => {
		await queue.getByRole('button', { name: `More actions for ${title}`, exact: true }).click();
		await page.getByRole('menuitem', { name: 'Play now', exact: true }).click();
	};
	const requestedGrants = () => grants.map((request) => request.postDataJSON());
	await page.goto(origin + library.path);
	for (const { id } of catalogue) {
		await page.getByTestId(library.item).and(page.locator(`[${library.id}="${id}"]`)).locator(library.select).click();
	}
	await expect.poll(queueIds).toEqual(catalogue.map(({ id }) => id));
	await expect(selected).toHaveAttribute(player.queueId, initial.id);
	await expect(audio).toHaveCount(0);
	const sources = await observeMediaSources(page);

	try {
		const [firstRequest] = await Promise.all([page.waitForRequest(playsUrl), choose(earlier.title)]);
		const { session_id: tab } = firstRequest.postDataJSON();
		const firstGrant = { track_id: earlier.id, session_id: tab };
		const secondGrant = { track_id: latest.id, session_id: tab };
		await choose(latest.title);
		expect(requestedGrants()).toEqual([firstGrant]);

		const earlierResponse = page.waitForResponse((response) => response.request() === firstRequest);
		const latestRequest = page.waitForRequest((request) =>
			request.url() === playsUrl && request.postDataJSON()?.track_id === latest.id);
		releaseEarlier.resolve();
		const [firstResponse, secondRequest] = await Promise.all([earlierResponse, latestRequest]);
		expect(firstResponse.status()).toBe(200);
		await expect(selected).toHaveAttribute(player.queueId, initial.id);
		await expect(audio).toHaveCount(0);
		expect(secondRequest.postDataJSON()).toEqual(secondGrant);

		const latestResponse = page.waitForResponse((response) => response.request() === secondRequest);
		releaseLatest.resolve();
		expect((await latestResponse).status()).toBe(200);
		const path = `${player.plays}/media/${latest.id}/${tab}/${latestPlayId}`;
		await expect(selected).toHaveAttribute(player.queueId, latest.id);
		await expect(page.getByTestId('library-now-playing')).toHaveText(latest.title);
		await expect.poll(() => audio.evaluate((node: HTMLAudioElement, url) =>
			node.currentSrc === url && !node.paused && !node.error && node.currentTime > 0.2, origin + path)).toBe(true);
		await expect.poll(queueIds).toEqual(catalogue.map(({ id }) => id));
		expect(requestedGrants()).toEqual([firstGrant, secondGrant]);
		// Compare both requests on the same browser clock, including the gap after releaseEarlier.
		const timings = await page.evaluate((url) => performance.getEntriesByName(url, 'resource')
			.filter((entry): entry is PerformanceResourceTiming => entry instanceof PerformanceResourceTiming)
			.map(({ startTime, responseEnd }) => ({ startTime, responseEnd })), playsUrl);
		expect(timings).toHaveLength(2);
		const [earlierTiming, latestTiming] = timings;
		expect(earlierTiming.responseEnd).toBeGreaterThan(earlierTiming.startTime);
		expect(latestTiming.startTime).toBeGreaterThanOrEqual(earlierTiming.responseEnd);
		expect([...mediaRequests]).toEqual([origin + path]);
		expect(await sources.evaluate((seen) => [...seen])).toEqual([path]);
	} finally {
		releaseEarlier.resolve();
		releaseLatest.resolve();
		await sources.dispose();
	}
});
