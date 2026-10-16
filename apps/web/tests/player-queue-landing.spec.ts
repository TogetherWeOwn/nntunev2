import { expect, test, type Page } from '@playwright/test';

/**
 * Landing pins for PR #79 (player queue with prev/next + Media Session).
 * Test files only; implementation lands via the paired landing card.
 *
 * PR #79's own `player-queue.spec.ts` covers play-through, reorder +
 * persistence, and metadata-follows-track, but leaves three observable
 * behaviours unpinned (verified against the PR head `3180429`):
 *   1. Previous on the first track is a no-op (does not resume/repoint).
 *   2. `aria-current` marks exactly the current queue item's select button.
 *   3. Clearing the queue clears MediaSession (no stale OS controls).
 *
 * Test command: `pnpm --dir=apps/web exec playwright test tests/player-queue-landing.spec.ts`
 * Expected failure before implementation: RED — `/demo/player` does not
 * exist on main yet, so `queue-item` never reaches 3 (route 404s). After
 * the paired landing card merges #79, this file must go GREEN unmodified.
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
	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
}

test('previous on the first track is a no-op; aria-current tracks the current item', async ({
	page
}) => {
	await openPlayer(page);

	const audio = page.getByTestId('demo-audio');
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	// Exactly the current item is marked.
	await expect(page.getByTestId('queue-select-demo-track')).toHaveAttribute(
		'aria-current',
		'true'
	);
	await expect(page.getByTestId('queue-select-second-signal')).not.toHaveAttribute(
		'aria-current',
		'true'
	);

	// Previous at the first track: stays put, stream untouched.
	await page.getByTestId('player-prev').click();
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	await expect(audio).toHaveAttribute('src', '/api/demo/audio');
	await expect(page.getByTestId('player-status')).not.toContainText('ended');

	// The mark follows the track after transport.
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await expect(page.getByTestId('queue-select-second-signal')).toHaveAttribute(
		'aria-current',
		'true'
	);
	await expect(page.getByTestId('queue-select-demo-track')).not.toHaveAttribute(
		'aria-current',
		'true'
	);
});

test('clearing the queue clears MediaSession metadata', async ({ page }) => {
	await openPlayer(page);

	// Sanity: metadata is published while tracks remain.
	await expect
		.poll(() => page.evaluate(() => navigator.mediaSession?.metadata?.title ?? null))
		.toBe('Demo track');

	await page.getByTestId('queue-clear').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');

	// OS controls must not linger on the emptied player.
	await expect
		.poll(() => page.evaluate(() => navigator.mediaSession?.metadata ?? null))
		.toBeNull();
});
