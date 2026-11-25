import { fileURLToPath } from 'node:url';
import { expect, test as base } from '@playwright/test';
import { preview } from 'vite';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';

const test = base.extend<{ origin: string }>({
	origin: async ({}, use) => {
		// Arm the production Library playback path on a dedicated browser-fixtures preview.
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
			await use(`http://127.0.0.1:${address.port}`);
		} finally {
			await server.close();
		}
	}
});

test.use({ serviceWorkers: 'block' });

test('the latest invited Library selection wins while playback is loading', async ({ page, context, origin }) => {
	const { library, player } = PRIVATE_MUSIC;
	const catalogue = [
		{ id: '00000000-0000-4000-8000-000000000001', title: 'First Tone', duration_ms: 14_000 },
		{ id: '00000000-0000-4000-8000-000000000002', title: 'Second Tone', duration_ms: 14_000 }
	] as const;
	const [first, second] = catalogue;
	const firstPlayId = '00000000-0000-4000-8000-000000000003';
	const secondPlayId = '00000000-0000-4000-8000-000000000004';
	const tones = privateStagingAudio();
	const audioByTrack = new Map<string, Buffer>([
		[first.id, tones.find(({ file }) => file === 'tone-a.flac')!.body],
		[second.id, tones.find(({ file }) => file === 'tone-b.flac')!.body]
	]);
	const cookie = { name: 'private_music_fixture_session', value: 'synthetic-listener', url: origin, httpOnly: true };
	const isListener = (header: string | null) => header?.split('; ').includes(`${cookie.name}=${cookie.value}`);
	const binding = 'A'.repeat(43);
	const releaseFirst = Promise.withResolvers<void>();
	const media = new Map<string, Buffer>();
	const grants: string[] = [];
	const requestedMedia = new Set<string>();
	const mediaRoot = `${player.plays}/media/`;

	await context.route('**/*', (route) => new URL(route.request().url()).origin === origin ? route.continue() : route.abort());
	await context.addCookies([cookie]);
	await page.route(`${origin}/v1/me/likes/session`, async (route) => {
		if (!isListener(await route.request().headerValue('cookie'))) return route.fulfill({ status: 401 });
		return route.fulfill({ json: { session_binding: binding } });
	});
	await page.route(`${origin}${library.api}`, async (route) => {
		if (!isListener(await route.request().headerValue('cookie'))) return route.fulfill({ status: 401 });
		return route.fulfill({ headers: { 'x-nntune-session-binding': binding }, json: catalogue });
	});
	await page.route(`${origin}${player.plays}`, async (route) => {
		const call = route.request();
		const { track_id, session_id, play_id } = call.postDataJSON();
		const track = catalogue.find(({ id }) => id === track_id);
		if (!track || call.method() !== 'POST' || !isListener(await call.headerValue('cookie')) ||
			!/^[0-9a-f-]{36}$/.test(session_id) || play_id !== undefined ||
			await call.headerValue('x-nntune-browser-playback') !== '1' ||
			await call.headerValue('x-nntune-session-binding') !== binding) return route.fulfill({ status: 400 });
		grants.push(track.id);
		const playId = track === first ? firstPlayId : secondPlayId;
		if (track === first) await releaseFirst.promise;
		media.set(`${mediaRoot}${track.id}/${session_id}/${playId}`, audioByTrack.get(track.id)!);
		return route.fulfill({ json: {
			trackId: track.id, playbackSessionId: session_id, sessionBinding: binding, playId, durationMs: track.duration_ms
		} });
	});
	await page.route(`${origin}${mediaRoot}**`, async (route) => {
		const call = route.request();
		const path = new URL(call.url()).pathname;
		requestedMedia.add(path);
		const body = media.get(path);
		if (!body || !isListener(await call.headerValue('cookie'))) return route.fulfill({ status: 404 });
		return route.fulfill({ contentType: 'audio/flac', body });
	});

	const rows = page.getByTestId(library.item);
	const row = (id: string) => rows.and(page.locator(`[${library.id}="${id}"]`));
	const queue = page.getByTestId(player.queueItem);
	const active = page.locator(player.currentQueueItem);
	const audio = page.getByTestId(player.audio);
	const nowPlaying = page.getByTestId('library-now-playing');
	const playNow = async (track: typeof catalogue[number]) => {
		await row(track.id).getByRole('button', { name: `More actions for ${track.title}`, exact: true }).click();
		await page.getByRole('menuitem', { name: 'Play now', exact: true }).click();
	};

	await page.goto(origin + library.path);
	for (const track of catalogue) await row(track.id).locator(library.select).click();
	await expect(active).toHaveAttribute(player.queueId, first.id);
	await expect(audio).toHaveCount(0);

	// Keep even a short-lived source assignment that never requests media.
	const sources = await page.evaluateHandle((testId) => {
		const seen = new Set<string>();
		const selector = `audio[data-testid="${testId}"]`;
		const record = (node: Element) => {
			const src = node.getAttribute('src');
			if (src) seen.add(src);
		};
		new MutationObserver((entries) => {
			for (const entry of entries) {
				if (entry.target instanceof Element && entry.target.matches(selector) && entry.oldValue) seen.add(entry.oldValue);
				for (const node of entry.addedNodes) {
					if (!(node instanceof Element)) continue;
					if (node.matches(selector)) record(node);
					node.querySelectorAll(selector).forEach(record);
				}
			}
			document.querySelectorAll(selector).forEach(record);
		}).observe(document, { childList: true, subtree: true, attributes: true, attributeFilter: ['src'], attributeOldValue: true });
		return seen;
	}, player.audio);

	try {
		const pending = page.waitForRequest((request) => request.url() === `${origin}${player.plays}` &&
			request.method() === 'POST' && request.postDataJSON().track_id === first.id);
		await playNow(first);
		const firstRequest = await pending;
		await expect(audio).toHaveCount(0);
		await playNow(second);
		releaseFirst.resolve();
		const delayed = await firstRequest.response();
		expect(delayed?.status()).toBe(200);
		expect(await delayed?.finished()).toBeNull();

		const secondSource = `${mediaRoot}${second.id}/${firstRequest.postDataJSON().session_id}/${secondPlayId}`;
		await expect(audio).toHaveCount(1);
		expect(await audio.getAttribute('src')).toBe(secondSource);
		await expect.poll(() => audio.evaluate((node: HTMLAudioElement) => ({
			source: node.currentSrc, advancing: !node.paused && node.currentTime > 0.2
		}))).toEqual({ source: origin + secondSource, advancing: true });
		await expect(nowPlaying).toHaveText(second.title);
		await expect(active).toHaveAttribute(player.queueId, second.id);
		expect(await queue.evaluateAll((items, attr) => items.map((item) => item.getAttribute(attr)), player.queueId))
			.toEqual([first.id, second.id]);
		expect(grants).toEqual([first.id, second.id]);
		expect([...requestedMedia]).toEqual([secondSource]);
		expect(await sources.evaluate((seen) => [...seen])).toEqual([secondSource]);
	} finally {
		releaseFirst.resolve();
		await sources.dispose();
	}
});
