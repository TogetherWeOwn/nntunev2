import { expect, test, type Page } from '@playwright/test';

/**
 * Acceptance (mocked browser): a MediaMetadata construction failure for
 * one track must not stop Media Session controls from updating to that
 * track, must not surface as an uncaught page error, and must not break
 * the queue. All media responses are mocked and every non-local request
 * is aborted; no real artwork/backend request leaves the page.
 *
 * The fixed demo catalogue cannot carry a malformed artworkUrl, so the
 * failure is injected at the same seam the malformed URL hits in
 * production: `new MediaMetadata(...)` throws for the second track.
 */

type CapturedState = {
	handlers: Map<string, (() => void) | null>;
	registrations: number;
};

async function openPlayer(page: Page, failMetadataForTitle: string | null): Promise<string[]> {
	const pageErrors: string[] = [];
	page.on('pageerror', (error) => pageErrors.push(error.message));

	// Intercept everything: synthetic audio/artwork locally, abort the rest.
	await page.route('**/*', (route) => {
		const url = new URL(route.request().url());
		if (url.hostname !== 'localhost' && url.hostname !== '127.0.0.1') {
			return route.abort();
		}
		if (url.pathname.startsWith('/api/demo/audio')) {
			return route.fulfill({
				status: 200,
				contentType: 'audio/wav',
				headers: { 'accept-ranges': 'bytes' },
				body: Buffer.alloc(2048, 7)
			});
		}
		if (url.pathname.startsWith('/api/demo/artwork')) {
			return route.fulfill({
				status: 200,
				contentType: 'image/svg+xml',
				body: '<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"></svg>'
			});
		}
		return route.continue();
	});

	await page.addInitScript((failingTitle) => {
		const captured: CapturedState = { handlers: new Map(), registrations: 0 };
		(window as unknown as { __mediaState: CapturedState }).__mediaState = captured;

		if ('mediaSession' in navigator) {
			const original = navigator.mediaSession.setActionHandler.bind(navigator.mediaSession);
			navigator.mediaSession.setActionHandler = (action, handler) => {
				captured.registrations += 1;
				captured.handlers.set(action, handler as (() => void) | null);
				try {
					original(action, handler);
				} catch {
					// Headless engines may reject some actions; capture is the point.
				}
			};
		}

		if (failingTitle && typeof window.MediaMetadata === 'function') {
			const RealMediaMetadata = window.MediaMetadata;
			window.MediaMetadata = class extends RealMediaMetadata {
				constructor(init?: MediaMetadataInit) {
					if (init?.title === failingTitle) {
						throw new Error(`synthetic MediaMetadata failure for ${failingTitle}`);
					}
					super(init);
				}
			};
		}
	}, failMetadataForTitle);

	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	return pageErrors;
}

async function registrationCount(page: Page): Promise<number> {
	return page.evaluate(
		() => (window as unknown as { __mediaState: CapturedState }).__mediaState.registrations
	);
}

async function invokeHandler(page: Page, action: string): Promise<void> {
	await page.evaluate((name) => {
		const state = (window as unknown as { __mediaState: CapturedState }).__mediaState;
		const handler = state.handlers.get(name);
		if (!handler) throw new Error(`${name} handler not wired`);
		handler();
	}, action);
}

async function metadataTitle(page: Page): Promise<string | null> {
	return page.evaluate(() => navigator.mediaSession?.metadata?.title ?? null);
}

test('metadata construction failure does not strand Media Session controls', async ({ page }) => {
	const pageErrors = await openPlayer(page, 'Second Signal');

	// Track 1 applies normally.
	await expect.poll(() => metadataTitle(page)).toBe('Demo track');
	const registrationsForFirstTrack = await registrationCount(page);
	expect(registrationsForFirstTrack).toBeGreaterThan(0);

	// Move to the track whose MediaMetadata construction throws.
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');

	// Controls must be re-registered for the new track despite the failure:
	// stale handlers from the previous track are not an acceptable outcome.
	await expect
		.poll(() => registrationCount(page))
		.toBeGreaterThan(registrationsForFirstTrack);

	// The OS next handler captured for the second track advances the queue.
	await invokeHandler(page, 'nexttrack');
	await expect(page.getByTestId('now-playing')).toContainText('Third Light');

	// Metadata recovers on the next good track, and OS previous works.
	await expect.poll(() => metadataTitle(page)).toBe('Third Light');
	await invokeHandler(page, 'previoustrack');
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');

	// The construction failure never escaped as an uncaught page error.
	expect(pageErrors).toEqual([]);
});

test('queue and Media Session stay healthy without injected failures (positive control)', async ({
	page
}) => {
	const pageErrors = await openPlayer(page, null);

	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	await expect.poll(() => metadataTitle(page)).toBe('Demo track');

	await invokeHandler(page, 'nexttrack');
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await expect.poll(() => metadataTitle(page)).toBe('Second Signal');

	await page.getByTestId('queue-clear').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
	// Null-track teardown clears metadata.
	await expect.poll(() => metadataTitle(page)).toBeNull();

	expect(pageErrors).toEqual([]);
});
