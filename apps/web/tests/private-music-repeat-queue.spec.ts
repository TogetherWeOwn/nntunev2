import { expect, type Request } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';
import { fixtureCookie, hasFixtureSession, serveFlac, test } from './helpers/private-music-fixture.js';

test('invited Library Repeat Queue follows the edited order and wraps with a fresh grant', async ({ page, context, origin }) => {
	test.setTimeout(60_000);
	const { library, player: contract } = PRIVATE_MUSIC;
	const grantUrl = origin + contract.plays;
	const listener = 'synthetic-listener';
	const binding = 'A'.repeat(43);
	const [toneA, toneB] = privateStagingAudio();
	const [start, wrap, tail] = [
		{ title: 'Starting Tone', body: toneA!.body },
		{ title: 'Wrap Tone', body: toneA!.body },
		{ title: 'Tail Tone', body: toneB!.body }
	].map(({ title, body }, index) => ({
		id: `00000000-0000-4000-8000-00000000000${index + 1}`, title, duration_ms: 14_000,
		body
	}));
	const catalogue = [start, wrap, tail];
	const edited = [wrap, start, tail];
	const playId = (number: number) => `private_browser_${String(number).padStart(22, '0')}`;
	const mediaPath = (trackId: string, tab: string, number: number) =>
		`${contract.plays}/media/${trackId}/${tab}/${playId(number)}`;
	const isListener = async (call: Request) => hasFixtureSession(await call.headerValue('cookie'), listener);
	const grantRequests: Request[] = [];
	const mediaRequests: { path: string; authorized: boolean }[] = [];
	const releaseGrants = [Promise.withResolvers<void>(), Promise.withResolvers<void>()];
	let grantsIssued = 0;
	let activeMedia: { path: string; body: Buffer } | undefined;

	await context.route('**/*', (route) => new URL(route.request().url()).origin === origin ? route.continue() : route.abort());
	await context.addCookies([{ name: fixtureCookie, value: listener, url: origin, httpOnly: true }]);
	await page.route(`${origin}/v1/me/likes/session`, async (route) => await isListener(route.request())
		? route.fulfill({ json: { session_binding: binding } }) : route.fulfill({ status: 401 }));
	await page.route(origin + library.api, async (route) => {
		if (!await isListener(route.request())) return route.fulfill({ status: 401 });
		return route.fulfill({ headers: { 'x-nntune-session-binding': binding },
			json: catalogue.map(({ id, title, duration_ms }) => ({ id, title, duration_ms })) });
	});
	page.on('request', (call) => {
		if (call.url() === grantUrl) grantRequests.push(call);
	});
	await page.route(grantUrl, async (route) => {
		const call = route.request();
		if (call.method() !== 'POST' || !await isListener(call) ||
			await call.headerValue('x-nntune-browser-playback') !== '1' ||
			await call.headerValue('x-nntune-session-binding') !== binding) return route.fulfill({ status: 400 });
		const { track_id, session_id, play_id } = call.postDataJSON() ?? {};
		const track = catalogue.find(({ id }) => id === track_id);
		if (!track || typeof session_id !== 'string' || !/^[0-9a-f-]{36}$/.test(session_id) ||
			play_id !== undefined) return route.fulfill({ status: 400 });
		const number = ++grantsIssued;
		activeMedia = undefined;
		await releaseGrants[number - 2]?.promise;
		activeMedia = { path: mediaPath(track.id, session_id, number), body: track.body };
		return route.fulfill({ json: {
			trackId: track.id, playbackSessionId: session_id, sessionBinding: binding,
			playId: playId(number), durationMs: track.duration_ms
		} });
	});
	await page.route(`${grantUrl}/media/**`, async (route) => {
		const call = route.request();
		const media = activeMedia;
		const path = call.url().slice(origin.length);
		const authorized = call.method() === 'GET' && media?.path === path && await isListener(call);
		mediaRequests.push({ path, authorized });
		if (!authorized) return route.fulfill({ status: 404 });
		return serveFlac(route, media.body);
	});

	const rows = page.getByTestId(library.item);
	const queue = page.getByTestId(contract.queueItem);
	const active = page.locator(contract.currentQueueItem);
	const player = page.getByRole('region', { name: 'Player', exact: true });
	const audio = page.getByTestId(contract.audio);
	const queueIds = () => queue.evaluateAll((items, attr) => items.map((item) => item.getAttribute(attr)), contract.queueId);
	const expectPlaying = async (track: typeof start, path: string) => {
		await expect(active).toHaveAttribute(contract.queueId, track.id);
		await expect(player.getByTestId('library-now-playing')).toHaveText(track.title);
		await expect(audio).toHaveAttribute('src', path);
		await expect.poll(() => audio.evaluate((node: HTMLAudioElement, source) =>
			node.currentSrc === source && !node.paused && !node.ended && !node.error &&
			node.currentTime > 0.2 && node.currentTime < 4, origin + path
		), `${track.title} plays from its new receipt near the start`).toBe(true);
	};

	await page.goto(origin + library.path);
	for (const { id } of catalogue) {
		await rows.and(page.locator(`[${library.id}="${id}"]`)).locator(library.select).click();
	}
	await expect.poll(queueIds).toEqual(catalogue.map(({ id }) => id));
	await page.getByRole('button', { name: `Move ${wrap.title} up`, exact: true }).click();
	await expect.poll(queueIds).toEqual(edited.map(({ id }) => id));
	await expect(active).toHaveAttribute(contract.queueId, start.id);
	await page.getByLabel('Repeat', { exact: true }).selectOption('queue');
	await expect(page.getByTestId('repeat-status')).toHaveText('Repeat: Queue');
	await expect(audio).toHaveCount(0);
	expect(grantRequests).toHaveLength(0);

	// Observe the Chromium media lifecycle, including replay from already buffered bytes.
	const observed = await page.evaluateHandle((audioId) => {
		const plays: string[] = [];
		const loads: string[] = [];
		const ends: { source: string; at: number; position: number; duration: number; trusted: boolean }[] = [];
		for (const type of ['play', 'loadstart', 'ended']) {
			document.addEventListener(type, (event) => {
				const audio = event.target;
				if (!(audio instanceof HTMLAudioElement) || audio.dataset.testid !== audioId) return;
				if (event.type === 'play') plays.push(audio.currentSrc || audio.src);
				else if (event.type === 'loadstart') loads.push(audio.currentSrc || audio.src);
				else ends.push({ source: audio.currentSrc, at: event.timeStamp, position: audio.currentTime,
					duration: audio.duration, trusted: event.isTrusted });
			}, true);
		}
		return { plays, loads, ends };
	}, contract.audio);
	const expectSources = async (paths: string[]) => {
		const { plays, loads } = await observed.jsonValue();
		const sources = paths.map((path) => origin + path);
		expect(plays, 'native play attempts use the authorized sources once each').toEqual(sources);
		expect(loads, 'native loads use the authorized sources once each').toEqual(sources);
		expect([...new Set(mediaRequests.map(({ path }) => path))]).toEqual(paths);
		expect(mediaRequests.filter(({ authorized }) => !authorized), 'media attempts outside the active grant').toEqual([]);
	};

	try {
		const initialGrant = page.waitForRequest(grantUrl);
		await player.getByRole('button', { name: 'Play', exact: true }).click();
		const tab: string = (await initialGrant).postDataJSON().session_id;
		const played = [start, tail, wrap];
		const paths = played.map(({ id }, index) => mediaPath(id, tab, index + 1));
		await expectPlaying(start, paths[0]!);

		// Each completion reaches the next track without any transport action.
		for (const [index, release] of releaseGrants.entries()) {
			const previous = played[index]!;
			const target = played[index + 1]!;
			await expect.poll(async () => (await observed.jsonValue()).ends.length, {
				message: `${previous.title} finishes naturally`, timeout: 18_000
			}).toBe(index + 1);
			const end = (await observed.jsonValue()).ends[index]!;
			expect(end.source).toBe(origin + paths[index]);
			expect(end.trusted).toBe(true);
			expect(end.duration).toBeCloseTo(previous.duration_ms / 1000, 2);
			expect(end.position).toBeCloseTo(end.duration, 2);
			await expect.poll(() => grantsIssued, 'the next authorized grant is pending').toBe(index + 2);
			expect(grantRequests[index + 1]?.postDataJSON()).toEqual({ track_id: target.id, session_id: tab });
			await expect(active).toHaveAttribute(contract.queueId, previous.id);
			await expectSources(paths.slice(0, index + 1));
			release.resolve();
			await expectPlaying(target, paths[index + 1]!);
			const grantStarts = await page.evaluate((url) => performance.getEntriesByName(url, 'resource')
				.map(({ startTime }) => startTime), grantUrl);
			expect(grantStarts[index + 1], 'the new grant request starts after native completion').toBeGreaterThanOrEqual(end.at);
		}
		await expectSources(paths);
		await expect.poll(queueIds).toEqual(edited.map(({ id }) => id));
		expect(grantRequests.map((call) => call.postDataJSON())).toEqual(played.map(({ id }) => ({ track_id: id, session_id: tab })));
	} finally {
		for (const release of releaseGrants) release.resolve();
	}
});
