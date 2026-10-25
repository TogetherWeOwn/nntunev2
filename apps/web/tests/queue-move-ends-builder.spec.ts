import { expect, test, type Page } from '@playwright/test';

const tracks = [
	{ id: 'demo-track', title: 'Demo track', artist: 'Fixture Band' },
	{ id: 'second-signal', title: 'Second Signal', artist: 'Fixture Band' },
	{ id: 'third-light', title: 'Third Light', artist: 'Session Players' }
];

// Each interaction owns its observer window. Assert every commit (including an
// empty one) before disconnecting; filter outcomes must not be discarded just
// because the neighbouring assertion concerns a move.
async function observeFeedback(page: Page) {
	const probe = await page.getByTestId('queue-announcement').evaluateHandle((region) => {
		const messages: string[] = [];
		const observer = new MutationObserver(() => messages.push(region.textContent?.trim() ?? ''));
		observer.observe(region, { childList: true, characterData: true, subtree: true });
		return { messages, observer };
	});
	return async (expected: string[]) => {
		expect(await probe.evaluate(({ messages, observer }) => {
			observer.disconnect();
			return messages;
		})).toEqual(expected);
		await probe.dispose();
	};
}

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
			const calls = { play: 0, pause: 0 };
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
		} else {
			await page.waitForFunction(() => typeof (window as unknown as { __seekStorm?: unknown }).__seekStorm === 'function');
		}
		await expect(page.getByTestId('queue-item')).toHaveCount(3);
		const transportBefore = await page.evaluate(() => {
			const { play, pause } = (window as unknown as {
				queueMoveProbe: { play: number; pause: number };
			}).queueMoveProbe;
			return { play, pause };
		});

		const expectFilterCommits = await observeFeedback(page);
		await page.getByRole('searchbox', { name: 'Filter queue', exact: true }).fill('Third Light');
		await expect(page.getByTestId('queue-item')).toHaveCount(1);
		if (surface === 'demo') {
			await expect(page.getByTestId('queue-announcement')).toHaveText('1 of 3 tracks');
		} else {
			await expect(page.getByTestId('queue-filter-count')).toHaveText('1 of 3 tracks');
		}
		await expectFilterCommits(surface === 'demo' ? ['1 of 3 tracks'] : []);

		const expectTopCommits = await observeFeedback(page);
		const top = page.getByRole('button', { name: 'Move Third Light to top', exact: true });
		await expect(top).toBeEnabled();
		await top.focus();
		await expect(top).toBeFocused();
		await page.keyboard.press('Enter');
		await expect(page.getByTestId('queue-announcement')).toHaveText('Third Light moved to 1 of 3');
		// Native disabled-boundary focus behavior is unchanged from Up/Down.
		await expect(top).toBeDisabled();
		await top.click({ force: true });
		await expectTopCommits(['Third Light moved to 1 of 3']);

		const expectClearFilterCommits = await observeFeedback(page);
		await page.getByRole('button', { name: 'Clear queue filter', exact: true }).click();
		if (surface === 'demo') {
			await expect(page.getByTestId('queue-announcement')).toHaveText('3 of 3 tracks');
		} else {
			await expect(page.getByTestId('queue-filter-count')).toHaveText('3 of 3 tracks');
		}
		await expectClearFilterCommits(surface === 'demo' ? ['3 of 3 tracks'] : []);
		expect(await page.getByTestId('queue-item').evaluateAll((rows) =>
			rows.map((row) => row.getAttribute('data-track-id'))
		)).toEqual(['third-light', 'demo-track', 'second-signal']);

		const expectBottomCommits = await observeFeedback(page);
		await page.getByRole('button', { name: 'Move Demo track to bottom', exact: true }).click();
		await expect(page.getByTestId('queue-announcement')).toHaveText('Demo track moved to 3 of 3');
		await expectBottomCommits(['Demo track moved to 3 of 3']);
		if (surface === 'library') {
			await expect(page.locator('[data-testid="queue-item"][aria-current="true"]'))
				.toHaveAttribute('data-track-id', 'demo-track');
		} else {
			await expect(page.getByTestId('queue-select-demo-track')).toHaveAttribute('aria-current', 'true');
		}
		const probe = await page.evaluate(() => (window as unknown as {
			queueMoveProbe: { play: number; pause: number };
		}).queueMoveProbe);
		expect(probe).toEqual(transportBefore);
		expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);

		const expectClearCommits = await observeFeedback(page);
		await page.getByRole('button', { name: 'Clear queue', exact: true }).click();
		await expect(page.getByTestId('queue-announcement')).not.toContainText('moved to');
		await expect(page.getByTestId('queue-announcement')).toHaveText(
			surface === 'library' ? 'Queue cleared.' : '0 of 0 tracks'
		);
		await expectClearCommits([surface === 'library' ? 'Queue cleared.' : '0 of 0 tracks']);
	});
}
