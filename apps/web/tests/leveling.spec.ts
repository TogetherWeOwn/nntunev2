import { expect, test, type Page } from '@playwright/test';

/**
 * Loudness leveling over the demo player, mocked at the gain node: the page
 * exposes `__levelingProbe` (enabled/track/linear/contextState) so the specs
 * assert the resolved gain per fixture track without measuring real audio
 * output, and pin that playback resumes a suspended leveling context.
 */

interface LevelingProbe {
	enabled: boolean;
	trackId: string | null;
	linear: number;
	contextState: string;
}

const STORAGE_KEY = 'nntune.leveling.enabled';

const EXPECTED_LINEAR: Record<string, number> = {
	'demo-track': Math.pow(10, -3 / 20),
	'second-signal': Math.pow(10, 4 / 20),
	'third-light': Math.pow(10, -1.5 / 20)
};

async function levelingProbe(page: Page): Promise<LevelingProbe> {
	return page.evaluate(() => {
		const probe = (window as unknown as { __levelingProbe?: () => LevelingProbe })
			.__levelingProbe;
		if (!probe) throw new Error('leveling probe missing');
		return probe();
	});
}

/**
 * Wait for client hydration: the probe only exists once Svelte mounts, so
 * specs that interact before any probe poll must wait for it first —
 * otherwise SSR markup can satisfy assertions while clicks get lost.
 */
async function waitForLevelingProbe(page: Page): Promise<void> {
	await page.waitForFunction(
		() =>
			typeof (window as unknown as { __levelingProbe?: unknown }).__levelingProbe ===
			'function'
	);
}

/** Poll until the probe reports the track with its leveled gain. */
async function expectLeveled(page: Page, trackId: string): Promise<void> {
	const expected = EXPECTED_LINEAR[trackId] as number;
	await expect
		.poll(async () => {
			const probe = await levelingProbe(page);
			return probe.trackId === trackId && Math.abs(probe.linear - expected) < 1e-9;
		})
		.toBe(true);
}

test('leveling defaults to on with per-fixture leveled gains', async ({ page }) => {
	await page.goto('/demo/player');

	const toggle = page.getByTestId('leveling-toggle');
	await expect(toggle).toBeVisible();
	await expect(toggle).toHaveAttribute('aria-pressed', 'true');

	await expectLeveled(page, 'demo-track');

	await page.getByTestId('player-next').click();
	await expectLeveled(page, 'second-signal');

	await page.getByTestId('player-next').click();
	await expectLeveled(page, 'third-light');

	// Leveling is active, not unity: fixture gains differ per track.
	const gains = Object.values(EXPECTED_LINEAR);
	expect(Math.max(...gains) - Math.min(...gains)).toBeGreaterThan(0.5);
});

test('disabling the toggle drives unity gain and persists across reload', async ({ page }) => {
	await page.goto('/demo/player');
	await waitForLevelingProbe(page);

	const toggle = page.getByTestId('leveling-toggle');
	await expect(toggle).toHaveAttribute('aria-pressed', 'true');

	await toggle.click();
	await expect(toggle).toHaveAttribute('aria-pressed', 'false');
	await expect.poll(async () => (await levelingProbe(page)).linear).toBe(1);

	await expect
		.poll(async () => page.evaluate((key) => localStorage.getItem(key), STORAGE_KEY))
		.toBe('0');

	await page.reload();
	await expect(page.getByTestId('leveling-toggle')).toHaveAttribute('aria-pressed', 'false');
	await expect.poll(async () => (await levelingProbe(page)).linear).toBe(1);

	await page.getByTestId('leveling-toggle').click();
	await expect(page.getByTestId('leveling-toggle')).toHaveAttribute('aria-pressed', 'true');
	await expectLeveled(page, 'demo-track');
});

test('toggle is keyboard reachable with its state announced', async ({ page }) => {
	await page.goto('/demo/player');
	await waitForLevelingProbe(page);

	const toggle = page.getByTestId('leveling-toggle');
	await expect(toggle).toHaveAccessibleName('Loudness leveling');

	await toggle.focus();
	await page.keyboard.press('Space');
	await expect(toggle).toHaveAttribute('aria-pressed', 'false');

	await page.keyboard.press('Enter');
	await expect(toggle).toHaveAttribute('aria-pressed', 'true');
});

test('starting playback resumes the leveling context', async ({ page }) => {
	await page.addInitScript(() => {
		const NativeAudioContext = window.AudioContext;
		let context: AudioContext | undefined;
		window.AudioContext = class extends NativeAudioContext {
			constructor(...args: ConstructorParameters<typeof AudioContext>) {
				super(...args);
				context = this;
			}
		};
		(window as unknown as { __suspendLevelingContext: () => Promise<void> })
			.__suspendLevelingContext = async () => {
			if (!context) throw new Error('leveling context missing');
			await context.suspend();
		};
	});
	await page.goto('/demo/player');
	await waitForLevelingProbe(page);
	await expectLeveled(page, 'demo-track');

	// Suspend the real context even when headless autoplay starts it running.
	// The hook is test-local; neither context state nor resume is mocked.
	await page.evaluate(() =>
		(window as unknown as { __suspendLevelingContext: () => Promise<void> })
			.__suspendLevelingContext()
	);
	await expect.poll(async () => (await levelingProbe(page)).contextState).toBe('suspended');

	// Selecting the same track leaves the leveling effect idle, so only the
	// play-path retries can resume the graph. Removing them must fail here.
	await page.getByTestId('queue-select-demo-track').click();
	await expect.poll(async () => (await levelingProbe(page)).contextState).toBe('running');
});
