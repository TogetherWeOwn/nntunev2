import { expect, test } from '@playwright/test';

// Failing-first smoke test for TOG-25177 (built on TOG-25185).
// The Builder implements the `/player` route; this file must NOT be edited
// on the build card — only the route/player implementation may change.
//
// Contract pinned here: the route serves synthetic media with these hooks:
// - `audio[data-testid="player-audio"]` — the media element
// - `[data-testid="player-seek"]` — range input driving seeks
// - `[data-testid="player-state"]` — text: idle | seeking | ready
// - `[data-testid="player-position"]` — text: current seconds (number)
// - `[data-testid="player-error"]` — present ONLY on failure
//
// A 10x rapid seek storm must abort superseded loads and settle the player
// at the final position (never wedged in `seeking`, never an error).

test('10x rapid seeks settle the player at the final position', async ({ page }) => {
	const response = await page.goto('/player?synthetic=1');
	expect(response?.ok(), '/player route should exist').toBe(true);

	const audio = page.getByTestId('player-audio');
	const seek = page.getByTestId('player-seek');
	const state = page.getByTestId('player-state');
	const position = page.getByTestId('player-position');

	await expect(audio, 'synthetic media should attach').toBeAttached();
	await expect(state, 'player should become ready').toContainText(/ready/i);

	// Storm: 10 sequential seeks fired as fast as the harness can issue them.
	for (let step = 1; step <= 10; step += 1) {
		await seek.fill(String(step * 5));
	}

	// The player must recover: final position wins, state leaves `seeking`.
	await expect(position, 'final seek position should win').toContainText('50', {
		timeout: 15_000
	});
	await expect(state, 'player should settle out of seeking').not.toContainText(/seeking/i, {
		timeout: 15_000
	});
	await expect(
		page.getByTestId('player-error'),
		'no error state after a seek storm'
	).toHaveCount(0);
});
