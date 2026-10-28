import { expect, test, type Page } from '@playwright/test';

/**
 * Builder-owned row-action pins complementing the protected
 * `demo-queue-filter.acceptance.spec.ts` bank. This file is safe for the
 * Builder to extend; the bank itself stays byte-identical.
 *
 * The demo panel renders each visible row with its ORIGINAL queue index, so
 * select, move and the disabled/current markers must ignore the filtered
 * row positions. These vectors prove that while a filter is active: a
 * regression using the visible position (selectAt(0) / moveTrack(0, -1) on
 * a lone 'third-light' row) must fail here. Mirrors the library
 * component's QFILTER-B02 (filtered actions hit arranged tracks) and
 * QFILTER-C13 (boundaries ignore filtered positions) coverage.
 *
 * UI: native textbox/searchbox "Filter queue", button "Clear queue filter".
 * Live role=status text contains "N of M tracks". Keep queue-item,
 * data-track-id, now-playing and existing transport hooks.
 *
 * Offline setup: pnpm --dir apps/web install --offline --frozen-lockfile --prod=false
 * Run: CI=1 PUBLIC_API_BASE_URL='' pnpm --dir apps/web exec playwright test \
 *        tests/demo-queue-filter-builder.spec.ts --workers=1
 * Chromium must already be installed. Local host may require LD_LIBRARY_PATH
 * pointing at its provisioned browser libraries. Port 4173 must be free, or
 * supply a run-owned config with an unused strict port; never reuse a preview.
 * All network is loopback-only, audio held locally; no backend/prod/provider.
 */

const ORDER = ['demo-track', 'second-signal', 'third-light'];
const input = (page: Page) => page.getByRole('textbox', { name: 'Filter queue', exact: true })
	.or(page.getByRole('searchbox', { name: 'Filter queue', exact: true }));
const clear = (page: Page) => page.getByRole('button', { name: 'Clear queue filter', exact: true });
const order = (page: Page) => page.getByTestId('queue-item').evaluateAll((rows) => rows.map((row) => row.getAttribute('data-track-id')));
const settle = (page: Page) => page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
const currentId = (page: Page) => page.evaluate(() => JSON.parse(sessionStorage.getItem('nntune:demo-queue:v1') ?? 'null')?.currentId);

test.setTimeout(20_000);
test.use({ hasTouch: true });

async function open(page: Page, baseURL: string) {
	const origin = new URL(baseURL);
	expect(['localhost', '127.0.0.1', '[::1]']).toContain(origin.hostname);
	await page.route('**/*', (route) => {
		const target = new URL(route.request().url());
		if (target.origin !== origin.origin) return route.abort();
		// Keep browser media pending locally; deterministic media doubles below
		// isolate the external decoder, not the real playback state machine.
		if (target.pathname === '/api/demo/audio') return;
		if (target.pathname.startsWith('/api/')) return route.abort();
		return route.continue();
	});
	await page.addInitScript(() => {
		HTMLMediaElement.prototype.play = function () {
			Object.defineProperty(this, 'paused', { value: false, configurable: true });
			Object.defineProperty(this, 'ended', { value: false, configurable: true });
			return Promise.resolve();
		};
		HTMLMediaElement.prototype.pause = function () {
			Object.defineProperty(this, 'paused', { value: true, configurable: true });
		};
	});
	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	// SSR rows alone are insufficient: this real mutation proves hydration.
	await page.getByTestId('queue-move-up-third-light').click();
	await expect.poll(() => order(page)).toEqual(['demo-track', 'third-light', 'second-signal']);
	await page.getByTestId('queue-move-down-third-light').click();
	await expect.poll(() => order(page)).toEqual(ORDER);
	await settle(page);
}

async function live(page: Page) {
	return (await page.getByRole('status').allTextContents()).join(' ').replace(/\s+/g, ' ');
}

test('filtered demo move uses the original queue index and full-queue boundaries', async ({ page, baseURL }) => {
	await open(page, baseURL!);
	await input(page).fill('third');
	await settle(page);
	await expect.poll(() => order(page)).toEqual(['third-light']);
	// Full index 2 is the last row, even though it is visible position 0.
	await expect(page.getByTestId('queue-move-down-third-light')).toBeDisabled();
	await expect(page.getByTestId('queue-move-up-third-light')).toBeEnabled();
	await page.getByTestId('queue-move-up-third-light').click();
	// The move lands in the arranged queue; the filter itself is untouched.
	await expect(input(page)).toHaveValue('third');
	await expect(page.getByTestId('queue-move-down-third-light')).toBeEnabled();
	await expect.poll(() => order(page)).toEqual(['third-light']);
	expect(await live(page)).toContain('1 of 3 tracks');
	await clear(page).click();
	await expect.poll(() => order(page)).toEqual(['demo-track', 'third-light', 'second-signal']);
	// First-row edge: full index 0 pins the opposite boundary pair.
	await input(page).fill('demo');
	await settle(page);
	await expect.poll(() => order(page)).toEqual(['demo-track']);
	await expect(page.getByTestId('queue-move-up-demo-track')).toBeDisabled();
	await expect(page.getByTestId('queue-move-down-demo-track')).toBeEnabled();
});

test('filtered demo select marks and persists the original queue track', async ({ page, baseURL }) => {
	await open(page, baseURL!);
	await input(page).fill('third');
	await settle(page);
	await expect.poll(() => order(page)).toEqual(['third-light']);
	await page.getByTestId('queue-select-third-light').click();
	// A visible-position regression would select demo-track (full index 0).
	await expect(page.getByTestId('queue-select-third-light')).toHaveAttribute('aria-current', 'true');
	await expect.poll(() => currentId(page)).toBe('third-light');
	await expect(page.getByTestId('now-playing')).toContainText('Third Light');
	await expect(input(page)).toHaveValue('third');
	expect(await live(page)).toContain('1 of 3 tracks');
	await clear(page).click();
	await expect.poll(() => order(page)).toEqual(ORDER);
	await expect.poll(() => currentId(page)).toBe('third-light');
	await expect(page.getByTestId('queue-select-third-light')).toHaveAttribute('aria-current', 'true');
	await expect(page.getByTestId('now-playing')).toContainText('Third Light');
});

test('filtered demo remove deletes the original queue track, not the visible position', async ({ page, baseURL }) => {
	await open(page, baseURL!);
	await page.getByTestId('queue-select-demo-track').click();
	await expect.poll(() => currentId(page)).toBe('demo-track');
	// Hide the first row so the visible target sits at visible position 0
	// with full-queue index 1.
	await input(page).fill('second');
	await settle(page);
	await expect.poll(() => order(page)).toEqual(['second-signal']);
	await page.getByTestId('queue-remove-second-signal').click();
	// A visible-position regression would delete demo-track (full index 0)
	// and advance the current selection away from it.
	await expect.poll(() => order(page)).toEqual([]);
	await expect(input(page)).toHaveValue('second');
	expect(await live(page)).toContain('0 of 2 tracks');
	await clear(page).click();
	await expect.poll(() => order(page)).toEqual(['demo-track', 'third-light']);
	await expect.poll(() => currentId(page)).toBe('demo-track');
	await expect(page.getByTestId('queue-select-demo-track')).toHaveAttribute('aria-current', 'true');
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
});
