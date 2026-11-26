import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';
import { mockInvitedLibraryPlayback } from './fixtures/invited-library-playback.js';
import { playQueuedTrack, serveFlac, test } from './helpers/private-music-fixture.js';

type DelayedSeekAudio = HTMLAudioElement & {
	delaySeeked?: () => void;
	releaseSeeked?: () => { trusted: boolean; retired: boolean; aborted: boolean; delivered: number };
};

test('a delayed private seek completion leaves the replacement seek pending', async ({ page, origin }) => {
	const { library, player } = PRIVATE_MUSIC;
	const [first, next] = ['First Tone', 'Second Tone'].map((title, i) => ({
		id: `00000000-0000-4000-8000-00000000000${i + 1}`, title, duration_ms: 14_000
	}));
	const tracks = [first, next];
	const [toneA, toneB] = privateStagingAudio();
	const fixture = await mockInvitedLibraryPlayback(page, origin, {
		libraryItems: () => tracks,
		playableTrackId: (id) => tracks.find((track) => track.id === id)?.id,
		audio: (route) => {
			const path = new URL(route.request().url()).pathname;
			return serveFlac(route, path.startsWith(`/v1/plays/media/${next.id}/`) ? toneB.body : toneA.body);
		}
	});
	await page.addInitScript((audioTestId) => {
		const add = HTMLAudioElement.prototype.addEventListener;
		let firstAudio: DelayedSeekAudio | undefined;
		let holding = false;
		let completion: Event | undefined;
		const queued: { listener: EventListener; signal: AbortSignal }[] = [];
		HTMLAudioElement.prototype.addEventListener = function (this: DelayedSeekAudio, type: string,
			listener: EventListenerOrEventListenerObject, options?: boolean | AddEventListenerOptions) {
			const signal = typeof options === 'object' && options.signal;
			if (type === 'seeked' && typeof listener === 'function' && signal && this.dataset.testid === audioTestId) {
				firstAudio ??= this;
				if (this === firstAudio) {
					this.delaySeeked = () => { holding = true; };
					this.releaseSeeked = () => {
						if (!completion?.isTrusted || !queued.length) throw new Error('Missing queued native seek completion');
						const trusted = completion.isTrusted;
						const retired = !this.isConnected;
						const callbacks = queued.splice(0);
						const aborted = callbacks.every((callback) => callback.signal.aborted);
						let delivered = 0;
						// Deliver the already captured callbacks with the original target and currentTarget.
						add.call(this, 'seeked', (event) => {
							for (const callback of callbacks) {
								callback.listener.call(this, event);
								delivered++;
							}
						}, { once: true });
						this.dispatchEvent(completion);
						return { trusted, retired, aborted, delivered };
					};
					add.call(this, type, (event) => {
						if (!holding) return listener.call(this, event);
						completion = event;
						queued.push({ listener, signal });
						this.dataset.heldSeeked = String(event.isTrusted);
					}, options);
					return;
				}
			}
			add.call(this, type, listener, options);
		};
	}, player.audio);

	const transport = page.getByRole('region', { name: 'Player', exact: true });
	const audio = transport.getByTestId(player.audio);
	const seek = transport.getByRole('slider', { name: 'Seek', exact: true });
	const play = transport.getByRole('button', { name: 'Play', exact: true });
	const active = page.locator(player.currentQueueItem);
	const pause = async (track: typeof first) => {
		await expect(active).toHaveAttribute(player.queueId, track.id);
		await expect(audio).toHaveJSProperty('currentSrc', origin + fixture.mediaPath(track.id));
		await expect.poll(() => audio.evaluate((node: HTMLAudioElement) => !node.paused && node.currentTime > 0)).toBe(true);
		await transport.getByRole('button', { name: 'Pause', exact: true }).click();
		await expect(play).toBeEnabled();
		await expect(seek).toBeEnabled();
		await expect(audio).toHaveJSProperty('seeking', false);
	};

	await fixture.signIn();
	await page.goto(origin + library.path);
	for (const track of tracks) {
		await page.getByTestId(library.item).filter({ hasText: track.title }).locator(library.select).click();
	}
	const [firstGrant] = await Promise.all([
		page.waitForResponse(origin + player.plays),
		play.click()
	]);
	expect(firstGrant.ok()).toBe(true);
	expect(firstGrant.request().postDataJSON()).toMatchObject({ track_id: first.id });
	await pause(first);
	const retired = await audio.elementHandle();
	await retired.evaluate((node: DelayedSeekAudio) => node.delaySeeked!());
	await transport.getByRole('button', { name: 'Forward 10 seconds' }).click();
	await expect(audio).toHaveAttribute('data-held-seeked', 'true');
	const oldPosition = await retired.evaluate((node: HTMLAudioElement) => node.currentTime);

	const choice = await playQueuedTrack(page, origin, next.title);
	expect(choice.ok()).toBe(true);
	await pause(next);
	const replacement = await audio.elementHandle();
	const control = await seek.elementHandle();
	const probe = await replacement.evaluateHandle((element, { old, control }) => {
		const node = element as HTMLAudioElement;
		const slider = control as HTMLInputElement;
		const read = () => ({
			position: node.currentTime, seeking: node.seeking, paused: node.paused, nativeControls: node.controls,
			source: node.currentSrc, mediaError: node.error?.code ?? null,
			value: slider.valueAsNumber, status: slider.getAttribute('aria-valuetext'), enabled: !slider.disabled,
			focused: document.activeElement === slider, connected: node.isConnected && slider.isConnected
		});
		const during = new Promise<{ before: ReturnType<typeof read>; after: ReturnType<typeof read>;
			delivery: ReturnType<NonNullable<DelayedSeekAudio['releaseSeeked']>> }>((resolve, reject) => {
			const observer = new MutationObserver(() => {
				const before = read();
				if (!before.status?.startsWith('Seeking to ')) return;
				observer.disconnect();
				// Replay after the pending UI renders, in the same turn as the seek.
				try {
					const delivery = (old as DelayedSeekAudio).releaseSeeked!();
					queueMicrotask(() => resolve({ before, after: read(), delivery }));
				} catch (error) { reject(error); }
			});
			observer.observe(slider, { attributes: true, attributeFilter: ['aria-valuetext'] });
		});
		const completed = new Promise<{ trusted: boolean; before?: ReturnType<typeof read>; after: ReturnType<typeof read> }>((resolve) => {
			// Only the replacement's seeked event should publish a settled
			// position here. Chromium can deliver timeupdate before it.
			const gate = new AbortController();
			for (const type of ['timeupdate', 'seeking', 'canplay', 'progress', 'durationchange']) {
				node.addEventListener(type, (event) => {
					if (!node.seeking) event.stopImmediatePropagation();
				}, { capture: true, signal: gate.signal });
			}
			let before: ReturnType<typeof read> | undefined;
			node.addEventListener('seeked', () => { before = read(); }, { capture: true, once: true });
			// This listener is registered after the player's seeked handlers.
			node.addEventListener('seeked', (event) => {
				const trusted = event.isTrusted;
				queueMicrotask(() => {
					gate.abort();
					resolve({ trusted, before, after: read() });
				});
			}, { once: true });
		});
		return { during, completed };
	}, { old: retired, control });

	const target = Number(await seek.inputValue()) + 1;
	expect(Math.abs(oldPosition - target)).toBeGreaterThan(1);
	await seek.press('ArrowRight');
	const during = await probe.evaluate(({ during }) => during);
	expect(during.delivery).toMatchObject({ trusted: true, retired: true, aborted: true });
	expect(during.delivery.delivered).toBeGreaterThan(0);
	const ready = { paused: true, nativeControls: true, enabled: true, focused: true, connected: true,
		source: origin + fixture.mediaPath(next.id), mediaError: null };
	expect(during.before).toMatchObject({ ...ready, seeking: true });
	expect(during.before.position).toBeCloseTo(target, 2);
	expect(during.before.value).toBeCloseTo(target, 2);
	expect(during.after).toEqual(during.before);

	const completed = await probe.evaluate(({ completed }) => completed);
	expect(completed.trusted).toBe(true);
	expect(completed.before?.status).toBe(during.before.status);
	expect(completed.after).toMatchObject({ ...ready, seeking: false,
		status: during.after.status?.replace('Seeking to ', '') });
	expect(completed.after.position).toBeCloseTo(target, 2);
	expect(completed.after.value).toBeCloseTo(target, 2);
	await expect(active).toHaveAttribute(player.queueId, next.id);
	await expect(transport.getByTestId('library-now-playing')).toHaveText(next.title);
	await expect(audio).toHaveJSProperty('currentSrc', origin + fixture.mediaPath(next.id));
	await expect(play).toBeEnabled();
	expect(await audio.evaluate((node, captured) => node === captured, replacement)).toBe(true);
});
