import { expect, test, type Page } from '@playwright/test';

/**
 * Builder-owned pins complementing the protected
 * `demo-play-next.spec.ts` bank. This file is safe for the Builder to
 * extend; the bank itself stays byte-identical.
 *
 * Radio owns what plays next (radio.order), so the demo Play next buttons
 * disable while radio is active — mirroring ShuffleUpcoming's radioActive
 * guard — and the handler no-ops even if invoked programmatically. These
 * vectors prove both layers: every row button is disabled during radio,
 * and a forced click changes neither the queue order nor the polite
 * announcement. Stopping radio re-enables the controls.
 * A filtered-row case also pins the original queue index: Third Light is
 * full index 2 but visible position 0, which would silently target current.
 *
 * Offline setup: pnpm --dir apps/web install --offline --frozen-lockfile --prod=false
 * Run: CI=1 PUBLIC_API_BASE_URL='' pnpm --dir apps/web exec playwright test \
 *        tests/demo-play-next-builder.spec.ts --workers=1
 * Chromium must already be installed. Local host may require LD_LIBRARY_PATH
 * pointing at its provisioned browser libraries. Port 4173 must be free, or
 * supply a run-owned config with an unused strict port; never reuse a preview.
 * All network is loopback-only, audio held locally; no backend/prod/provider.
 */

const ORDER = ['demo-track', 'second-signal', 'third-light'];
const SEED = 7;

async function openPlayer(page: Page): Promise<void> {
	await page.route('**/api/demo/audio**', () => {});
	await page.addInitScript(() => {
		HTMLMediaElement.prototype.play = function () {
			return Promise.resolve();
		};
		HTMLMediaElement.prototype.pause = function () {};
	});
	await page.goto(`/demo/player?radio-seed=${SEED}`);
	await expect(page.getByTestId('queue-item')).toHaveCount(ORDER.length);
}

async function naturalEnd(page: Page): Promise<void> {
	await page.getByTestId('demo-audio').evaluate((el) => {
		Object.defineProperty(el, 'paused', { value: true, configurable: true });
		Object.defineProperty(el, 'ended', { value: true, configurable: true });
		el.dispatchEvent(new Event('pause'));
		el.dispatchEvent(new Event('ended'));
	});
}

/** Play the seeded queue through natural ends until the radio badge shows. */
async function playThroughToRadio(page: Page): Promise<void> {
	await page.getByTestId('queue-select-demo-track').click();
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await naturalEnd(page);
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await naturalEnd(page);
	await expect(page.getByTestId('now-playing')).toContainText('Third Light');
	await naturalEnd(page);
	await expect(page.getByTestId('radio-badge')).toContainText('Radio');
}

const order = (page: Page) =>
	page
		.getByTestId('queue-item')
		.evaluateAll((rows) => rows.map((row) => row.getAttribute('data-track-id')));
const announcement = (page: Page) =>
	page.getByTestId('queue-panel').getByTestId('queue-announcement').textContent();

test.setTimeout(20_000);

test('filtered play next uses the original queue index rather than the visible position', async ({ page }) => {
	await openPlayer(page);
	// Metadata is published by a client effect, so this proves hydration.
	await expect.poll(() => page.evaluate(() => navigator.mediaSession.metadata?.title)).toBe('Demo track');
	const filter = page.getByRole('searchbox', { name: 'Filter queue', exact: true });
	await filter.fill('third');
	await expect.poll(() => order(page)).toEqual(['third-light']);

	// Third Light is full index 2, but visible position 0 would target the
	// current Demo track and silently no-op instead of moving Third Light.
	const control = page.getByTestId('queue-play-next-third-light');
	await control.click();
	const region = page.getByTestId('queue-panel').getByTestId('queue-announcement');
	await expect(region).toHaveAttribute('role', 'status');
	await expect(region).toContainText('Third Light');
	await expect(region).toContainText('position 2 of 3');
	await expect(control).toBeFocused();
	await expect(filter).toHaveValue('third');
	await expect.poll(() => order(page)).toEqual(['third-light']);

	await page.getByRole('button', { name: 'Clear queue filter', exact: true }).click();
	await expect.poll(() => order(page)).toEqual(['demo-track', 'third-light', 'second-signal']);
	await expect(page.getByTestId('queue-select-demo-track')).toHaveAttribute('aria-current', 'true');
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
});

test('play next disables during radio and forced clicks change nothing', async ({ page }) => {
	await openPlayer(page);
	await playThroughToRadio(page);

	for (const id of ORDER) {
		await expect(page.getByTestId(`queue-play-next-${id}`)).toBeDisabled();
	}
	const beforeOrder = await order(page);
	const beforeAnnouncement = await announcement(page);

	// dispatchEvent bypasses the disabled actionability check, pinning the
	// handler's own radio guard rather than the attribute alone. demo-track
	// is not current during radio, so playNext would move it without the guard.
	await page.getByTestId('queue-play-next-demo-track').dispatchEvent('click');
	await expect(page.getByTestId('radio-badge')).toContainText('Radio');
	expect(await order(page)).toEqual(beforeOrder);
	expect(await announcement(page)).toBe(beforeAnnouncement);

	await page.getByTestId('radio-stop').click();
	await expect(page.getByTestId('radio-badge')).toHaveCount(0);
	for (const id of ORDER) {
		await expect(page.getByTestId(`queue-play-next-${id}`)).toBeEnabled();
	}
});
