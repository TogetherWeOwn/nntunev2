import { expect, test } from '@playwright/test';

const tracks = [
	{ id: 'demo-track', title: 'Demo track', artist: 'Fixture Band' },
	{ id: 'second-signal', title: 'Second Signal', artist: 'Fixture Band' },
	{ id: 'third-light', title: 'Third Light', artist: 'Session Players' }
];

for (const surface of ['library', 'demo'] as const) {
	test(`${surface}: filtered end jumps use full indices, announce once and preserve transport`, async ({
		page,
		baseURL
	}) => {
		const origin = new URL(baseURL!).origin;
		expect(['localhost', '127.0.0.1', '[::1]']).toContain(new URL(origin).hostname);
		await page.setViewportSize({ width: 360, height: 800 });
		await page.addInitScript(() => {
			sessionStorage.clear();
			localStorage.clear();
			const calls = { play: 0, pause: 0, messages: [] as string[] };
			(window as unknown as { queueMoveProbe: typeof calls }).queueMoveProbe = calls;
			HTMLMediaElement.prototype.play = function () {
				calls.play++;
				return Promise.resolve();
			};
			HTMLMediaElement.prototype.pause = function () {
				calls.pause++;
			};
		});
		await page.route('**/*', (route) => {
			const url = new URL(route.request().url());
			if (url.origin !== origin) return route.abort();
			if (url.pathname === '/api/library') {
				return route.fulfill({ json: tracks });
			}
			if (url.pathname === '/api/demo/audio') return;
			return route.continue();
		});
		await page.goto(surface === 'library' ? '/library' : '/demo/player');
		if (surface === 'library') {
			for (const track of tracks) {
				await page.locator(`[data-testid="library-item"][data-id="${track.id}"]`).click();
			}
		}
		await expect(page.getByTestId('queue-item')).toHaveCount(3);
		await page.getByTestId('queue-announcement').evaluate((region) => {
			new MutationObserver(() => {
				const text = region.textContent?.trim();
				if (text) {
					(window as unknown as { queueMoveProbe: { messages: string[] } }).queueMoveProbe
						.messages.push(text);
				}
			}).observe(region, { childList: true, characterData: true, subtree: true });
		});
		const transportBefore = await page.evaluate(() => {
			const { play, pause } = (window as unknown as {
				queueMoveProbe: { play: number; pause: number };
			}).queueMoveProbe;
			return { play, pause };
		});

		await page.getByRole('searchbox', { name: 'Filter queue', exact: true }).fill('Third Light');
		await expect(page.getByTestId('queue-item')).toHaveCount(1);
		const top = page.getByRole('button', { name: 'Move Third Light to top', exact: true });
		await expect(top).toBeEnabled();
		await top.focus();
		await expect(top).toBeFocused();
		await page.keyboard.press('Enter');
		await expect(page.getByTestId('queue-announcement')).toHaveText('Third Light moved to 1 of 3');
		// Native disabled-boundary focus behavior is unchanged from Up/Down.
		await expect(top).toBeDisabled();
		await top.click({ force: true });

		await page.getByRole('button', { name: 'Clear queue filter', exact: true }).click();
		expect(await page.getByTestId('queue-item').evaluateAll((rows) =>
			rows.map((row) => row.getAttribute('data-track-id'))
		)).toEqual(['third-light', 'demo-track', 'second-signal']);
		await page.getByRole('button', { name: 'Move Demo track to bottom', exact: true }).click();
		await expect(page.getByTestId('queue-announcement')).toHaveText('Demo track moved to 3 of 3');
		if (surface === 'library') {
			await expect(page.locator('[data-testid="queue-item"][aria-current="true"]'))
				.toHaveAttribute('data-track-id', 'demo-track');
		} else {
			await expect(page.getByTestId('queue-select-demo-track')).toHaveAttribute('aria-current', 'true');
		}
		const probe = await page.evaluate(() => (window as unknown as {
			queueMoveProbe: { play: number; pause: number; messages: string[] };
		}).queueMoveProbe);
		expect({ play: probe.play, pause: probe.pause }).toEqual(transportBefore);
		expect(probe.messages).toEqual(['Third Light moved to 1 of 3', 'Demo track moved to 3 of 3']);
		expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);

		await page.getByRole('button', { name: 'Clear queue', exact: true }).click();
		if (surface === 'library') await page.getByRole('button', { name: 'Yes, clear queue', exact: true }).click();
		await expect(page.getByTestId('queue-announcement')).toHaveText(
			surface === 'library' ? 'Queue cleared.' : ''
		);
		expect(await page.evaluate(() => (window as unknown as {
			queueMoveProbe: { messages: string[] };
		}).queueMoveProbe.messages)).toEqual([
			'Third Light moved to 1 of 3', 'Demo track moved to 3 of 3',
			...(surface === 'library' ? ['Queue cleared.'] : [])
		]);
	});
}
