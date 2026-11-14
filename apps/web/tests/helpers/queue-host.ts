import { expect, type Page } from '@playwright/test';

/**
 * Open the persistent shell's queue surface for a production page.
 *
 * Production mounts the queue only after the visible Queue toggle opens the
 * shell panel (or the /queue route on a narrow viewport). Call this after the
 * test has entered the route under test, immediately before the first queue
 * assertion or interaction. It never swallows a failure: a missing toggle or a
 * panel that does not become visible fails the calling test.
 */
export async function openQueue(page: Page): Promise<void> {
	await expect(page.locator('html')).toHaveAttribute('data-hydrated', '');
	const panel = page.getByTestId('queue-panel');
	if ((await panel.count()) === 0) {
		const toggle = page.getByTestId('player-queue-toggle');
		await expect(toggle).toBeVisible();
		await toggle.click();
	}
	await expect(panel).toBeVisible();
}

/**
 * Close the shell's queue surface through its visible "Close queue" button.
 *
 * Lifecycle adapter for the narrow viewport, where openQueue navigates to
 * /queue and the caller's route (and its controls) unmounts. Only acts when
 * the queue panel exists. Asserts the panel is gone afterwards and, when
 * `restoredRoute` is given, that the caller's route is back with its
 * level-one heading visible. Focus returns to the shell's Queue toggle, so
 * callers must move focus themselves before sending keys.
 */
export async function closeQueue(
	page: Page,
	restoredRoute?: { url: RegExp | string; heading: string }
): Promise<void> {
	const panel = page.getByTestId('queue-panel');
	if ((await panel.count()) > 0) {
		const close = page.getByRole('button', { name: 'Close queue', exact: true });
		await expect(close).toBeVisible();
		await close.click();
	}
	await expect(panel).toHaveCount(0);
	if (restoredRoute) {
		await expect(page).toHaveURL(restoredRoute.url);
		await expect(page.getByRole('heading', { level: 1, name: restoredRoute.heading, exact: true })).toBeVisible();
	}
}
