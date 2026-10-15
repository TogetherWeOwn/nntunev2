import { expect, test, type Page } from '@playwright/test';

// Failing-first acceptance tests for the player-queue slice: prev/next,
// queue reorder and Media Session metadata. The builder implements the
// player to satisfy these specs without editing this file.
//
// Contract pinned here (builder: implement exactly these observable hooks;
// internals are yours to choose):
//
// Route: `/demo/player?synthetic=1` (the synthetic player page introduced
// by the player-resilience slice; no auth/provider/R2 involved). In
// synthetic mode it seeds a queue of exactly the three fixture tracks
// below, in that order, with the first track already current on load
// (audio may stay paused; only the current-track identity is pinned).
//
// Fixture tracks (stream URLs are the builder's choice, but each track
// must have a distinct one so play order is observable on the element):
//   1. { title: 'Alpha Sine', artist: 'Artist Alpha', album: 'Synthetic Fixtures', artwork: real URL }
//   2. { title: 'Beta Sine',  artist: 'Artist Beta',  album: 'Synthetic Fixtures', artwork: null }
//   3. { title: 'Gamma Sine', artist: 'Artist Gamma', album: 'Synthetic Fixtures', artwork: real URL }
//
// DOM hooks:
//   - `audio[data-testid="demo-audio"]` — the media element (existing
//     resilience-demo hook; its src/currentSrc follows the current track)
//   - `[data-testid="player-queue"]` — queue panel containing exactly one
//     `[data-testid="queue-item"]` per queued track, in play order; the
//     current track's item carries `aria-current="true"`, others do not
//   - `[data-testid="now-playing"]` — text naming the current track
//   - Buttons with accessible names matching /next track/i and
//     /previous track/i; per queue item, buttons whose accessible names
//     match /move .* up/i and /move .* down/i (keyboard-accessible reorder,
//     e.g. aria-label "Move Gamma Sine up")
//   - The media element advancing on `ended` to the next queued track
//
// Media Session: `navigator.mediaSession.metadata` mirrors the current
// track (title/artist/album/artwork) and `nexttrack` / `previoustrack`
// action handlers are wired to the same prev/next logic. A track with no
// artwork URL must publish the reused resilience placeholder (its src
// contains `placeholder-artwork`), never empty or missing artwork.

const FIXTURE_URL = '/demo/player?synthetic=1';

const TRACKS = [
	{ title: 'Alpha Sine', artist: 'Artist Alpha', album: 'Synthetic Fixtures' },
	{ title: 'Beta Sine', artist: 'Artist Beta', album: 'Synthetic Fixtures' },
	{ title: 'Gamma Sine', artist: 'Artist Gamma', album: 'Synthetic Fixtures' }
] as const;

type Track = (typeof TRACKS)[number];

async function openQueue(page: Page) {
	const response = await page.goto(FIXTURE_URL);
	expect(response?.ok(), 'synthetic player route should exist').toBe(true);
	await expect(
		page.getByTestId('queue-item'),
		'synthetic mode seeds the 3-track fixture queue'
	).toHaveCount(3);
}

async function expectQueueOrder(page: Page, titles: readonly string[]) {
	const items = page.getByTestId('queue-item');
	await expect(items).toHaveCount(titles.length);
	for (const [index, title] of titles.entries()) {
		await expect(items.nth(index), `queue position ${index}`).toContainText(title);
	}
}

async function expectCurrent(page: Page, track: Track) {
	await expect(
		page.getByTestId('now-playing'),
		`now-playing should name ${track.title}`
	).toContainText(track.title);
	const items = page.getByTestId('queue-item');
	for (const candidate of TRACKS) {
		const item = items.filter({ hasText: candidate.title }).first();
		if (candidate.title === track.title) {
			await expect(item, 'current queue item is marked').toHaveAttribute('aria-current', 'true');
		} else {
			await expect
				.poll(async () => item.getAttribute('aria-current'), {
					message: `${candidate.title} must not be marked current`
				})
				.not.toBe('true');
		}
	}
}

async function audioSrc(page: Page): Promise<string> {
	return page.getByTestId('demo-audio').evaluate((el) => {
		const audio = el as HTMLAudioElement;
		return audio.currentSrc || audio.src || audio.getAttribute('src') || '';
	});
}

async function expectAudioSrc(page: Page, expected: string) {
	await expect.poll(async () => audioSrc(page), { message: 'audio src follows track' }).toBe(expected);
}

interface SessionMetadata {
	title: string;
	artist: string;
	album: string;
	artwork: string[];
}

async function sessionMetadata(page: Page): Promise<SessionMetadata | null> {
	return page.evaluate(() => {
		const metadata = navigator.mediaSession?.metadata;
		if (!metadata) return null;
		return {
			title: metadata.title,
			artist: metadata.artist,
			album: metadata.album,
			artwork: (metadata.artwork ?? []).map((entry) => entry.src)
		};
	});
}

test('3-track queue plays in order with next/prev', async ({ page }) => {
	await openQueue(page);
	await expectQueueOrder(page, ['Alpha Sine', 'Beta Sine', 'Gamma Sine']);
	await expectCurrent(page, TRACKS[0]);

	// Each track maps to a distinct stream URL on the media element, and
	// returning to a track restores its URL (identity, not just position).
	const alphaSrc = await audioSrc(page);
	expect(alphaSrc, 'current track has a stream URL').not.toBe('');

	await page.getByRole('button', { name: /next track/i }).click();
	await expectCurrent(page, TRACKS[1]);
	await expect.poll(async () => audioSrc(page)).not.toBe(alphaSrc);
	const betaSrc = await audioSrc(page);
	expect(betaSrc).not.toBe('');

	await page.getByRole('button', { name: /next track/i }).click();
	await expectCurrent(page, TRACKS[2]);
	await expect.poll(async () => audioSrc(page)).not.toBe(betaSrc);
	const gammaSrc = await audioSrc(page);
	expect(gammaSrc).not.toBe('');
	expect(gammaSrc).not.toBe(alphaSrc);

	await page.getByRole('button', { name: /previous track/i }).click();
	await expectCurrent(page, TRACKS[1]);
	await expectAudioSrc(page, betaSrc);

	await page.getByRole('button', { name: /previous track/i }).click();
	await expectCurrent(page, TRACKS[0]);
	await expectAudioSrc(page, alphaSrc);

	// Play-through: when media ends, the player advances on its own.
	await page
		.getByTestId('demo-audio')
		.evaluate((el) => el.dispatchEvent(new Event('ended')));
	await expectCurrent(page, TRACKS[1]);
	await expectAudioSrc(page, betaSrc);
});

test('MediaSession metadata matches the current track', async ({ page }) => {
	// Record OS media handlers as the page registers them, so the spec can
	// prove nexttrack/previoustrack are wired (handlers are write-only via
	// the public API). The wrapper still calls through to the real API.
	await page.addInitScript(() => {
		const captured: Record<string, unknown> = {};
		Object.defineProperty(window, '__mediaSessionHandlers', { value: captured });
		const session = navigator.mediaSession;
		if (!session) return;
		const original = session.setActionHandler.bind(session);
		session.setActionHandler = (action, handler) => {
			captured[action] = handler;
			try {
				original(action, handler);
			} catch {
				// An unsupported action must not break capture of the rest.
			}
		};
	});

	await openQueue(page);
	await expectCurrent(page, TRACKS[0]);

	await expect
		.poll(async () => (await sessionMetadata(page))?.title, {
			message: 'mediaSession metadata publishes the current track'
		})
		.toBe(TRACKS[0].title);
	const alpha = await sessionMetadata(page);
	expect(alpha?.artist).toBe(TRACKS[0].artist);
	expect(alpha?.album).toBe(TRACKS[0].album);
	expect(alpha?.artwork.length, 'track with artwork publishes it').toBeGreaterThan(0);
	expect(alpha?.artwork[0]).toBeTruthy();
	expect(alpha?.artwork[0]).not.toContain('placeholder-artwork');

	// Metadata follows the track, and the artwork-less fixture track falls
	// back to the resilience placeholder instead of publishing no artwork.
	await page.getByRole('button', { name: /next track/i }).click();
	await expectCurrent(page, TRACKS[1]);
	await expect
		.poll(async () => (await sessionMetadata(page))?.title, {
			message: 'mediaSession metadata follows next'
		})
		.toBe(TRACKS[1].title);
	const beta = await sessionMetadata(page);
	expect(beta?.artist).toBe(TRACKS[1].artist);
	expect(beta?.album).toBe(TRACKS[1].album);
	expect(beta?.artwork.length, 'missing artwork still publishes artwork').toBeGreaterThan(0);
	expect(beta?.artwork[0]).toContain('placeholder-artwork');

	// OS controls: the recorded handlers drive the same queue.
	await page.evaluate(async () => {
		const handlers = (
			window as unknown as {
				__mediaSessionHandlers?: Record<
					string,
					((details: { action: string }) => void | Promise<void>) | undefined
				>;
			}
		).__mediaSessionHandlers;
		const next = handlers?.['nexttrack'];
		if (!next) throw new Error('mediaSession nexttrack handler is not wired');
		await next({ action: 'nexttrack' });
	});
	await expectCurrent(page, TRACKS[2]);
	await expect.poll(async () => (await sessionMetadata(page))?.title).toBe(TRACKS[2].title);

	await page.evaluate(async () => {
		const handlers = (
			window as unknown as {
				__mediaSessionHandlers?: Record<
					string,
					((details: { action: string }) => void | Promise<void>) | undefined
				>;
			}
		).__mediaSessionHandlers;
		const previous = handlers?.['previoustrack'];
		if (!previous) throw new Error('mediaSession previoustrack handler is not wired');
		await previous({ action: 'previoustrack' });
	});
	await expectCurrent(page, TRACKS[1]);
});

test('queue reorder changes play order', async ({ page }) => {
	await openQueue(page);
	await expectQueueOrder(page, ['Alpha Sine', 'Beta Sine', 'Gamma Sine']);
	await expectCurrent(page, TRACKS[0]);
	await expect.poll(async () => audioSrc(page)).not.toBe('');
	const alphaSrc = await audioSrc(page);

	// Move the last track to the front via its per-item move-up control.
	for (let move = 0; move < 2; move += 1) {
		await page
			.getByTestId('queue-item')
			.filter({ hasText: 'Gamma Sine' })
			.getByRole('button', { name: /move .* up/i })
			.click();
		await expectQueueOrder(
			page,
			move === 0 ? ['Alpha Sine', 'Gamma Sine', 'Beta Sine'] : ['Gamma Sine', 'Alpha Sine', 'Beta Sine']
		);
	}

	// Reordering must not restart or replace the playing track.
	await expectCurrent(page, TRACKS[0]);
	expect(await audioSrc(page), 'reorder keeps the current stream').toBe(alphaSrc);

	// Play order now follows the new queue: Alpha's successor is Beta, and
	// stepping back from Alpha reaches Gamma, the moved track.
	await page.getByRole('button', { name: /next track/i }).click();
	await expectCurrent(page, TRACKS[1]);

	await page.getByRole('button', { name: /previous track/i }).click();
	await expectCurrent(page, TRACKS[0]);

	await page.getByRole('button', { name: /previous track/i }).click();
	await expectCurrent(page, TRACKS[2]);
});
