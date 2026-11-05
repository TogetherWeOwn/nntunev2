import { expect, test, type Page } from '@playwright/test';

/**
 * Builder-owned: demo queue move controls (Up, Down, Top, Bottom) at initial
 * and reordered indices, keyboard focus through a reorder, and uninterrupted
 * native playback. The audio is the real /api/demo/audio WAV (no route mock,
 * no stubbed play/pause, no synthetic media events); the paused case never
 * starts it and the playing case starts it with a real click on the row.
 *
 *   pnpm --dir apps/web exec playwright test tests/demo-queue-move-controls-builder.spec.ts --workers=1
 */

const TITLES: Record<string, string> = {
	'demo-track': 'Demo track',
	'second-signal': 'Second Signal',
	'third-light': 'Third Light'
};

interface Control {
	text: string;
	label: string | null;
	testid: string | null;
	disabled: boolean;
}

async function openPlayer(page: Page): Promise<void> {
	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	// A client effect publishes MediaSession metadata only after hydration.
	await expect.poll(() => page.evaluate(() => navigator.mediaSession.metadata?.title)).toBe('Demo track');
}

/** Row ids with their move controls, read in DOM order (before the Next button). */
async function moveControls(page: Page): Promise<{ id: string | null; controls: Control[] }[]> {
	return page.getByTestId('queue-item').evaluateAll((rows) =>
		rows.map((row) => ({
			id: row.getAttribute('data-track-id'),
			controls: [...row.querySelectorAll('button')]
				.filter((button) => button.getAttribute('aria-label')?.startsWith('Move '))
				.map((button) => ({
					text: button.textContent?.trim() ?? '',
					label: button.getAttribute('aria-label'),
					testid: button.getAttribute('data-testid'),
					disabled: button.disabled
				}))
		}))
	);
}

/** Literal contract per row: fixed order and names, ends disabled, testids only on Up/Down. */
function expectedControls(order: string[]) {
	return order.map((id, index) => {
		const title = TITLES[id]!;
		const last = order.length - 1;
		return {
			id,
			controls: [
				{ text: 'Up', label: `Move ${title} up`, testid: `queue-move-up-${id}`, disabled: index === 0 },
				{ text: 'Down', label: `Move ${title} down`, testid: `queue-move-down-${id}`, disabled: index === last },
				{ text: 'Top', label: `Move ${title} to top`, testid: null, disabled: index === 0 },
				{ text: 'Bottom', label: `Move ${title} to bottom`, testid: null, disabled: index === last }
			]
		};
	});
}

async function audioState(page: Page) {
	return page.getByTestId('demo-audio').evaluate((node) => {
		const audio = node as HTMLAudioElement;
		return {
			same: (audio as unknown as { __probe?: boolean }).__probe === true,
			src: audio.getAttribute('src'),
			paused: audio.paused,
			currentTime: audio.currentTime,
			readyState: audio.readyState
		};
	});
}

for (const mode of ['paused', 'playing'] as const) {
	test(`demo move controls keep identity, keyboard focus and native playback while ${mode}`, async ({ page }) => {
		await openPlayer(page);
		const audio = page.getByTestId('demo-audio');
		await audio.evaluate((node) => {
			(node as unknown as { __probe: boolean }).__probe = true;
		});

		if (mode === 'playing') {
			await page.getByTestId('queue-select-demo-track').click();
			await expect.poll(async () => (await audioState(page)).paused).toBe(false);
			await expect.poll(async () => (await audioState(page)).currentTime).toBeGreaterThan(0);
		}

		// Initial indices: first row has no Up/Top, last row has no Down/Bottom.
		expect(await moveControls(page)).toEqual(expectedControls(['demo-track', 'second-signal', 'third-light']));

		const before = await audioState(page);
		expect(before.paused).toBe(mode === 'paused');

		// Keyboard: activate the last row's Up, then the (now moved) current row's Down.
		const thirdUp = page.getByTestId('queue-move-up-third-light');
		const thirdUpNode = await thirdUp.elementHandle();
		await thirdUp.focus();
		await page.keyboard.press('Enter');
		await expect(page.getByTestId('queue-item')).toHaveText([/Demo track/, /Third Light/, /Second Signal/]);
		expect(await moveControls(page)).toEqual(expectedControls(['demo-track', 'third-light', 'second-signal']));
		// Keyed row: the same control node survives and still holds focus.
		await expect(thirdUp).toBeFocused();
		expect(await thirdUp.evaluate((node, kept) => node === kept, thirdUpNode)).toBe(true);

		const demoDown = page.getByTestId('queue-move-down-demo-track');
		await demoDown.focus();
		await page.keyboard.press('Space');
		await expect(page.getByTestId('queue-item')).toHaveText([/Third Light/, /Demo track/, /Second Signal/]);
		expect(await moveControls(page)).toEqual(expectedControls(['third-light', 'demo-track', 'second-signal']));
		await expect(demoDown).toBeFocused();

		// The endpoint moves use the live original index too, not their loop slot.
		const demoRow = page.getByTestId('queue-item').filter({ has: page.getByTestId('queue-select-demo-track') });
		await demoRow.getByRole('button', { name: 'Move Demo track to top', exact: true }).click();
		expect(await moveControls(page)).toEqual(expectedControls(['demo-track', 'third-light', 'second-signal']));
		await demoRow.getByRole('button', { name: 'Move Demo track to bottom', exact: true }).click();
		expect(await moveControls(page)).toEqual(expectedControls(['third-light', 'second-signal', 'demo-track']));

		// Native playback continues: same element and source, same paused intent.
		await expect(page.getByTestId('now-playing')).toContainText('Demo track');
		const after = await audioState(page);
		expect(after).toMatchObject({ same: true, src: before.src, paused: before.paused });
		if (mode === 'paused') {
			expect(after.currentTime).toBe(before.currentTime);
		} else {
			expect(after.currentTime).toBeGreaterThanOrEqual(before.currentTime);
			await expect.poll(async () => (await audioState(page)).currentTime).toBeGreaterThan(after.currentTime);
			expect((await audioState(page)).paused).toBe(false);
		}
	});
}
