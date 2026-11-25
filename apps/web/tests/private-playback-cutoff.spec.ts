import { errors, expect, type Request } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';
import { fixtureCookie, hasFixtureSession, serveFlac, test } from './helpers/private-music-fixture.js';

test('an open private player refuses to switch tracks at cohort cutoff', async ({ page, context, origin }) => {
	const { library, player } = PRIVATE_MUSIC;
	const first = { id: '00000000-0000-4000-8000-000000000001', title: 'First private tone', duration_ms: 14_000 };
	const second = { id: '00000000-0000-4000-8000-000000000002', title: 'Second private tone', duration_ms: 14_000 };
	const catalogue = [first, second];
	const tones = privateStagingAudio();
	const firstPlayId = 'private_browser_000000000000000000000A';
	const secondPlayId = 'private_browser_000000000000000000000B';
	const listener = 'synthetic-listener';
	const binding = 'A'.repeat(43);
	const bound = async (request: Request) => hasFixtureSession(await request.headerValue('cookie'), listener);
	const playsUrl = origin + player.plays;
	const mediaPrefix = `${playsUrl}/media/`;
	// browser_play_issue returns the private_listening_deadline error once the cohort expires.
	const cutoff = { code: 'forbidden', message: 'private listening access required', retryable: false };
	let cutoffReached = false;
	const grantedMedia = new Map<string, Buffer>();
	const grants: Request[] = [];
	const mediaRequests = new Set<string>();
	page.on('request', (request) => {
		if (request.url() === playsUrl) grants.push(request);
		if (request.url().startsWith(mediaPrefix)) mediaRequests.add(request.url());
	});

	await context.route('**/*', (route) => new URL(route.request().url()).origin === origin ? route.continue() : route.abort());
	await context.addCookies([{ name: fixtureCookie, value: listener, url: origin, httpOnly: true }]);
	await page.route(`${origin}/v1/me/likes/session`, async (route) => await bound(route.request())
		? route.fulfill({ json: { session_binding: binding } }) : route.fulfill({ status: 401 }));
	await page.route(origin + library.api, async (route) => await bound(route.request())
		? route.fulfill({ headers: { 'x-nntune-session-binding': binding }, json: catalogue })
		: route.fulfill({ status: 401 }));
	await page.route(playsUrl, async (route) => {
		const request = route.request();
		if (request.method() !== 'POST' || !await bound(request) || await request.headerValue('origin') !== origin ||
			await request.headerValue('x-nntune-browser-playback') !== '1' ||
			await request.headerValue('x-nntune-session-binding') !== binding) return route.fulfill({ status: 400 });
		if (cutoffReached) return route.fulfill({ status: 403, json: cutoff });
		const { track_id, session_id, play_id } = request.postDataJSON();
		const track = catalogue.find(({ id }) => id === track_id);
		if (!track || !/^[0-9a-f-]{36}$/.test(session_id) || play_id !== undefined) return route.fulfill({ status: 400 });
		const playId = track === first ? firstPlayId : secondPlayId;
		const tone = tones.find(({ file }) => file === (track === first ? 'tone-a.flac' : 'tone-b.flac'))!;
		grantedMedia.set(`${mediaPrefix}${track.id}/${session_id}/${playId}`, tone.body);
		return route.fulfill({ json: {
			trackId: track.id, playbackSessionId: session_id, sessionBinding: binding, playId, durationMs: track.duration_ms
		} });
	});
	await page.route(`${mediaPrefix}**`, async (route) => {
		const body = grantedMedia.get(route.request().url());
		// Keep media already granted independent of the new-grant cutoff in this fixture.
		if (!body || !await bound(route.request())) return route.fulfill({ status: 404 });
		return serveFlac(route, body);
	});

	const transport = page.getByRole('region', { name: 'Player', exact: true });
	const audio = transport.getByTestId(player.audio);
	const queue = page.getByTestId(player.queueItem);
	const current = page.locator(player.currentQueueItem);
	await page.goto(origin + library.path);
	for (const track of catalogue) {
		await page.getByTestId(library.item).and(page.locator(`[${library.id}="${track.id}"]`)).locator(library.select).click();
	}
	await expect(queue).toHaveCount(2);
	await expect(current).toHaveAttribute(player.queueId, first.id);
	const [initialGrant] = await Promise.all([
		page.waitForResponse(playsUrl),
		transport.getByRole('button', { name: 'Play', exact: true }).click()
	]);
	expect(initialGrant.status()).toBe(200);
	const sessionId = initialGrant.request().postDataJSON().session_id;
	const originalSource = `${player.plays}/media/${first.id}/${sessionId}/${firstPlayId}`;
	await expect.poll(() => audio.evaluate((node: HTMLAudioElement) => ({
		source: node.currentSrc, playing: !node.paused && node.error === null && node.currentTime > 0.2
	}))).toEqual({ source: origin + originalSource, playing: true });

	// Capture transient src assignments as well as sources that make a media request.
	const sources = await page.evaluateHandle(() => {
		const seen = new Set<string>();
		const selector = 'audio[src], audio source[src]';
		const record = (node: Element) => {
			if (node.matches(selector)) seen.add(node.getAttribute('src')!);
			node.querySelectorAll(selector).forEach((source) => seen.add(source.getAttribute('src')!));
		};
		record(document.documentElement);
		new MutationObserver((entries) => {
			for (const entry of entries) {
				if (entry.target instanceof Element && entry.target.matches('audio, audio source')) {
					if (entry.oldValue) seen.add(entry.oldValue);
					record(entry.target);
				}
				for (const node of [...entry.addedNodes, ...entry.removedNodes]) {
					if (node instanceof Element) record(node);
				}
			}
		}).observe(document, { childList: true, subtree: true, attributeFilter: ['src'], attributeOldValue: true });
		return seen;
	});

	cutoffReached = true;
	await queue.getByRole('button', { name: `More actions for ${second.title}`, exact: true }).click();
	const [denial] = await Promise.all([
		page.waitForResponse(playsUrl),
		page.getByRole('menuitem', { name: 'Play now', exact: true }).click()
	]);
	expect(denial.status()).toBe(403);
	expect(denial.request().postDataJSON()).toEqual({ track_id: second.id, session_id: sessionId });
	const feedback = page.getByRole('status').and(page.getByTestId('queue-announcement'));
	await expect(feedback).toHaveText('Could not start playback.');
	await expect(transport.getByRole('button', { name: 'Retry playback', exact: true })).toHaveCount(0);
	await expect(page).toHaveURL(origin + library.path);
	await expect(current).toHaveAttribute(player.queueId, first.id);
	await expect(audio).toHaveAttribute('src', originalSource);

	// Observe a quiet interval after the failure is announced, then count all attempts
	// since navigation to catch retries dispatched before the feedback was rendered.
	await expect(page.waitForRequest((request) => request.url() === playsUrl, { timeout: 750 })).rejects.toThrow(errors.TimeoutError);
	expect(grants.map((request) => request.postDataJSON()), 'One accepted start and one denied switch, with no automatic retry').toEqual([
		{ track_id: first.id, session_id: sessionId }, { track_id: second.id, session_id: sessionId }
	]);
	expect([...mediaRequests]).toEqual([origin + originalSource]);
	expect(await sources.evaluate((seen) => [...seen])).toEqual([originalSource]);
	await sources.dispose();
});
