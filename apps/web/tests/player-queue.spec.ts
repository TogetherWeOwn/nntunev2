import { expect, test, type Page } from '@playwright/test';

/**
 * Queue fixture: all stream responses are mocked, so play-through is
 * driven by transport buttons and synthetic `ended` events (the fake
 * bytes never decode to real audio). MediaSession action handlers are
 * captured before page scripts run, then invoked exactly as the OS
 * would, proving the wiring instead of trusting it.
 */
async function openPlayer(page: Page): Promise<void> {
	await page.route('**/api/demo/audio**', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'audio/mpeg',
			headers: { 'accept-ranges': 'bytes' },
			body: Buffer.alloc(2048, 7)
		})
	);
	await page.addInitScript(() => {
		const captured = new Map<string, (() => void) | null>();
		(window as unknown as { __mediaHandlers: Map<string, (() => void) | null> }).__mediaHandlers =
			captured;
		if ('mediaSession' in navigator) {
			const original = navigator.mediaSession.setActionHandler.bind(navigator.mediaSession);
			navigator.mediaSession.setActionHandler = (action, handler) => {
				captured.set(action, handler as (() => void) | null);
				try {
					original(action, handler);
				} catch {
					// Headless engines may reject some actions; capture is the point.
				}
			};
		}
	});
	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
}

async function queueOrder(page: Page): Promise<(string | null)[]> {
	return page
		.getByTestId('queue-item')
		.evaluateAll((items) => items.map((item) => item.getAttribute('data-track-id')));
}

async function mediaMetadata(page: Page) {
	return page.evaluate(() => {
		const metadata = navigator.mediaSession?.metadata;
		if (!metadata) return null;
		return {
			title: metadata.title,
			artist: metadata.artist,
			artwork: metadata.artwork?.map((a) => a.src) ?? []
		};
	});
}

test('3-track queue plays through with next/prev and track end', async ({ page }) => {
	await openPlayer(page);

	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	const audio = page.getByTestId('demo-audio');
	await expect(audio).toHaveAttribute('src', '/api/demo/audio');

	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await expect(audio).toHaveAttribute('src', '/api/demo/audio?track=second-signal');

	// Track end advances the queue, like real playback finishing.
	await audio.evaluate((el: HTMLAudioElement) => el.dispatchEvent(new Event('ended')));
	await expect(page.getByTestId('now-playing')).toContainText('Third Light');

	// Next past the end stops instead of wrapping.
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Third Light');
	await expect(page.getByTestId('player-status')).toContainText('ended');

	await page.getByTestId('player-prev').click();
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	// Navigating away from the end clears the stale `ended` status.
	await expect(page.getByTestId('player-status')).not.toContainText('ended');
});

test('queue panel adds, clears and reorders; order persists in-session', async ({ page }) => {
	await openPlayer(page);
	expect(await queueOrder(page)).toEqual(['demo-track', 'second-signal', 'third-light']);

	// Reorder: third track up twice goes first; the playing track stays playing.
	await page.getByTestId('queue-move-up-third-light').click();
	await page.getByTestId('queue-move-up-third-light').click();
	expect(await queueOrder(page)).toEqual(['third-light', 'demo-track', 'second-signal']);
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');

	// In-session persistence: the reordered queue survives a reload.
	// Poll: SSR renders the default order before hydration restores it.
	await page.reload();
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect.poll(() => queueOrder(page)).toEqual(['third-light', 'demo-track', 'second-signal']);
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');

	// Clear, then add back in a chosen order.
	await page.getByTestId('queue-clear').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');

	await page.getByTestId('queue-add-second-signal').click();
	await page.getByTestId('queue-add-demo-track').click();
	expect(await queueOrder(page)).toEqual(['second-signal', 'demo-track']);
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	// Already-queued tracks cannot be added twice.
	await expect(page.getByTestId('queue-add-second-signal')).toBeDisabled();
});

test('MediaSession metadata and OS next/prev handlers follow the current track', async ({ page }) => {
	await openPlayer(page);

	// Track 1 has no art: MediaSession artwork must be the shared placeholder.
	// Poll: metadata is applied by a client effect after hydration.
	await expect.poll(async () => (await mediaMetadata(page))?.title).toBe('Demo track');
	const first = await mediaMetadata(page);
	expect(first?.title).toBe('Demo track');
	expect(first?.artist).toBe('Fixture Band');
	expect(first?.artwork[0]).toContain('placeholder-artwork.svg');

	// Invoke the captured OS handlers the way the OS would.
	await page.evaluate(() => {
		const handlers = (window as unknown as { __mediaHandlers: Map<string, (() => void) | null> })
			.__mediaHandlers;
		const next = handlers.get('nexttrack');
		if (!next) throw new Error('nexttrack handler not wired');
		next();
	});
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');

	await expect.poll(async () => (await mediaMetadata(page))?.title).toBe('Second Signal');
	const second = await mediaMetadata(page);
	expect(second?.title).toBe('Second Signal');
	expect(second?.artist).toBe('Fixture Band');
	expect(second?.artwork[0]).toContain('/api/demo/artwork');

	await page.evaluate(() => {
		const handlers = (window as unknown as { __mediaHandlers: Map<string, (() => void) | null> })
			.__mediaHandlers;
		const previous = handlers.get('previoustrack');
		if (!previous) throw new Error('previoustrack handler not wired');
		previous();
	});
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	await expect.poll(async () => (await mediaMetadata(page))?.title).toBe('Demo track');
});
