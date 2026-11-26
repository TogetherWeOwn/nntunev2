import { expect } from '@playwright/test';
import type { Connect } from 'vite';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';
import { mockInvitedLibraryPlayback } from './fixtures/invited-library-playback.js';
import { flacRange, hasFixtureSession, test as privateMusicTest } from './helpers/private-music-fixture.js';

const { library, player } = PRIVATE_MUSIC;
const tracks = [
	{ id: '00000000-0000-4000-8000-000000001201', title: 'Interrupted tone', duration_ms: 14_000 },
	{ id: '00000000-0000-4000-8000-000000001202', title: 'Upcoming tone', duration_ms: 14_000 }
];

function delayedAudio() {
	const tone = privateStagingAudio().find(({ file }) => file === 'tone-a.flac')!.body;
	// The first 80 KiB contain about six seconds of the 14-second tone.
	const prefixSize = 80 * 1024;
	const tail = Promise.withResolvers<void>();
	let authorizedPath: string | undefined;
	const middleware: Connect.NextHandleFunction = (request, response, next) => {
		if (!request.url?.startsWith(`${player.plays}/media/`)) return next();
		if (request.method !== 'GET' || request.url !== authorizedPath ||
			!hasFixtureSession(request.headers.cookie ?? null, 'synthetic-listener')) {
			response.writeHead(404).end();
			return;
		}
		const { status, start, bytes, headers } = flacRange(request.headers.range, tone);
		response.writeHead(status, { ...headers, 'content-type': 'audio/flac', 'content-length': bytes.length });
		const available = Math.max(0, prefixSize - start);
		if (bytes.length <= available) {
			response.end(bytes);
			return;
		}
		// Keep the response open and deliver the rest of the same valid range on demand.
		response.write(bytes.subarray(0, available));
		void tail.promise.then(() => {
			if (!response.destroyed) response.end(bytes.subarray(available));
		});
	};
	return {
		middleware, release: tail.resolve,
		authorize(path: string) { authorizedPath ??= path; }
	};
}

const test = privateMusicTest.extend<{ delayedMedia: ReturnType<typeof delayedAudio> }>({
	delayedMedia: async ({}, use) => { await use(delayedAudio()); },
	privateMediaMiddleware: async ({ delayedMedia }, use) => { await use(delayedMedia.middleware); }
});

for (const pauseDuringBuffering of [true, false]) {
	test(pauseDuringBuffering
		? 'private playback stays paused when buffered data arrives and resumes on Play'
		: 'private playback continues when buffered data arrives without a Pause', async ({ page, origin, delayedMedia }) => {
		const [selected] = tracks;
		const fixture = await mockInvitedLibraryPlayback(page, origin, {
			libraryItems: () => tracks,
			playableTrackId: (id) => tracks.find((track) => track.id === id)?.id,
			audio: (route) => {
				delayedMedia.authorize(new URL(route.request().url()).pathname);
				return route.continue();
			}
		});
		const grants: string[] = [];
		const mediaTargets = new Set<string>();
		page.on('request', (request) => {
			const url = new URL(request.url());
			if (url.pathname === player.plays) grants.push(request.postDataJSON()?.track_id);
			if (url.pathname.startsWith(`${player.plays}/media/`)) mediaTargets.add(url.href);
		});
		const transport = page.getByRole('region', { name: 'Player', exact: true });
		const play = transport.getByRole('button', { name: 'Play', exact: true });
		const pause = transport.getByRole('button', { name: 'Pause', exact: true });
		const audio = transport.getByTestId(player.audio);
		const queue = page.getByTestId(player.queueItem);
		const current = page.locator(player.currentQueueItem);

		await fixture.signIn();
		await page.goto(origin + library.path);
		for (const { id } of tracks) {
			await page.getByTestId(library.item).and(page.locator(`[${library.id}="${id}"]`)).locator(library.select).click();
		}
		await expect(current).toHaveAttribute(player.queueId, selected.id);
		try {
			await play.press('Enter');
			const native = await audio.evaluateHandle((node: HTMLAudioElement) => {
				const events = {
					waitingAt: null as number | null, pausedAt: null as number | null,
					resumeClick: null as { paused: boolean; position: number; alreadyResumed: boolean } | null,
					play: [] as number[], playing: [] as number[], interruptions: [] as string[]
				};
				node.addEventListener('waiting', (event) => {
					if (event.isTrusted && !node.paused && !node.seeking && !node.error &&
						node.currentTime > 0.5 && node.readyState < node.HAVE_FUTURE_DATA) {
						events.waitingAt ??= node.currentTime;
					}
				});
				node.addEventListener('pause', () => { events.pausedAt = node.currentTime; });
				for (const type of ['play', 'playing'] as const) {
					node.addEventListener(type, () => { if (events.waitingAt !== null) events[type].push(node.currentTime); });
				}
				for (const type of ['seeking', 'emptied', 'loadstart', 'ended', 'error']) {
					node.addEventListener(type, () => { if (events.waitingAt !== null) events.interruptions.push(type); });
				}
				return { node, events };
			});
			const source = origin + fixture.mediaPath(selected.id);
			await expect.poll(() => native.evaluate(({ events }) => events.waitingAt), {
				message: 'The native decoder must run out of data after playback starts', timeout: 15_000
			}).not.toBeNull();
			await expect(pause).toBeEnabled();
			await expect(audio).toHaveJSProperty('currentSrc', source);
			expect(await native.evaluate(({ node }) => !node.paused && !node.ended &&
				node.readyState < node.HAVE_FUTURE_DATA)).toBe(true);
			const waitingAt = (await native.evaluate(({ events }) => events.waitingAt))!;
			let resumeAt = waitingAt;
			if (pauseDuringBuffering) {
				await pause.press('Enter');
				await expect.poll(() => native.evaluate(({ events }) => events.pausedAt)).not.toBeNull();
				resumeAt = (await native.evaluate(({ events }) => events.pausedAt))!;
				// The clock may drain already-decoded output after `waiting`, but must not run on.
				expect(resumeAt).toBeGreaterThanOrEqual(waitingAt);
				expect(resumeAt - waitingAt).toBeLessThan(0.25);
			}

			delayedMedia.release();
			if (pauseDuringBuffering) {
				await expect.poll(() => native.evaluate(({ node }, position) =>
					node.readyState >= node.HAVE_FUTURE_DATA && Array.from({ length: node.buffered.length }, (_, index) => index)
						.some((index) => node.buffered.start(index) <= position && node.buffered.end(index) > position + 0.5), resumeAt), {
					message: 'Audio must be buffered beyond the paused point before pressing Play'
				}).toBe(true);
				await expect(play).toBeEnabled();
				await play.evaluate((button, { node, events }) => {
					// Observe the decoder at the trusted gesture, before the Player handles the click.
					button.addEventListener('click', (event) => {
						if (!event.isTrusted) return;
						events.resumeClick = {
							paused: node.paused, position: node.currentTime,
							alreadyResumed: events.play.length > 0 || events.playing.length > 0
						};
					}, { capture: true, once: true });
				}, native);
				await play.press('Enter');
				const click = await native.evaluate(({ events }) => events.resumeClick);
				expect(click).toMatchObject({ paused: true, alreadyResumed: false });
				expect(click?.position).toBeCloseTo(resumeAt, 1);
			}

			await expect.poll(() => native.evaluate(({ node, events }, position) =>
				node.isConnected && !node.paused && !node.ended && !node.error && events.playing.length > 0 &&
				node.currentTime > position + 0.25, resumeAt), 'the original decoder resumes and its clock advances').toBe(true);
			const events = await native.evaluate(({ events }) => events);
			expect(events.playing).toHaveLength(1);
			expect(Math.abs(events.playing[0] - resumeAt)).toBeLessThan(0.25);
			expect(events.interruptions).toEqual([]);
			if (pauseDuringBuffering) {
				expect(events.play).toHaveLength(1);
				expect(events.play[0]).toBeCloseTo(resumeAt, 1);
			} else {
				expect(events.play).toEqual([]);
				expect(events.pausedAt).toBeNull();
			}
			await expect(pause).toBeEnabled();
			await expect(audio).toHaveJSProperty('currentSrc', source);
			expect(await audio.evaluate((node, original) => node === original, await native.getProperty('node'))).toBe(true);
			await expect(current).toHaveAttribute(player.queueId, selected.id);
			await expect(transport.getByTestId('library-now-playing')).toHaveText(selected.title);
			expect(await queue.evaluateAll((items, attr) => items.map((item) => item.getAttribute(attr)), player.queueId))
				.toEqual(tracks.map(({ id }) => id));
			expect(grants).toEqual([selected.id]);
			expect([...mediaTargets]).toEqual([source]);
		} finally {
			delayedMedia.release();
		}
	});
}
