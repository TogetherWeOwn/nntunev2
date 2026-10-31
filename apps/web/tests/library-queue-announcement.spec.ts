import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Queue announcement hygiene on /library (QUEUE-MANAGE follow-up).
 *
 * The `queue-announcement` live region is host-owned page state. A move
 * sets the position text; remove and clear replace it with their own
 * concise outcome; every other queue edit clears it. Otherwise the
 * previous "moved to N of M" text goes stale (positions may no longer
 * exist) and a track title can linger across queue clears.
 *
 * Harness note (same as the neighbouring library specs): `/library` is
 * client-fetched (`ssr = false`), so the click-driven contract is pinned
 * here with a route-mocked `/api/library`.
 */

const ITEMS = [
	{ id: 'row-zulu', title: 'Zulu Song', artist: 'Queue Artist' },
	{ id: 'row-alpha', title: 'Alpha Song' },
	{ id: 'row-mike', title: 'Mike Song', artist: 'Queue Artist' }
];

async function seedLibrary(
	page: Page,
	payload: { current: unknown } = { current: ITEMS }
): Promise<void> {
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(payload.current)
		})
	);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
}

function row(page: Page, id: string): Locator {
	return page.locator(`[data-testid="library-item"][data-id="${id}"]`);
}

function queueItem(page: Page, id: string): Locator {
	return page.locator(
		`[data-testid="queue-item"][data-track-id="${id}"], [data-testid="queue-item"][data-id="${id}"]`
	);
}

function rowButton(page: Page, trackId: string, name: string): Locator {
	return queueItem(page, trackId).getByRole('button', { name, exact: true });
}

async function queueOrder(page: Page): Promise<(string | null)[]> {
	return page
		.getByTestId('queue-item')
		.evaluateAll((items) =>
			items.map((item) => item.getAttribute('data-track-id') ?? item.getAttribute('data-id'))
		);
}

/** Fill the queue in gesture order: mike, zulu, alpha. Current is mike. */
async function seedQueue(
	page: Page,
	payload: { current: unknown } = { current: ITEMS }
): Promise<void> {
	await seedLibrary(page, payload);
	await row(page, 'row-mike').click();
	await row(page, 'row-zulu').click();
	await row(page, 'row-alpha').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
}

/** Move alpha up, pinning both the reorder and the move announcement. */
async function moveAlphaUp(page: Page): Promise<Locator> {
	const moveUp = rowButton(page, 'row-alpha', 'Move Alpha Song up');
	await expect(moveUp).toBeVisible();
	await moveUp.click();
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha', 'row-zulu']);
	const announcement = page.getByTestId('queue-announcement');
	await expect(announcement).toContainText('Alpha Song');
	await expect(announcement).toContainText('2 of 3');
	return announcement;
}

test('row add clears a stale move announcement even for an already queued track', async ({ page }) => {
	await seedQueue(page);
	const announcement = await moveAlphaUp(page);

	await row(page, 'row-zulu').click();

	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha', 'row-zulu']);
	await expect(announcement).toHaveText('');
});

test('transport Next clears a stale move announcement', async ({ page }) => {
	await seedQueue(page);
	const announcement = await moveAlphaUp(page);

	await page.getByTestId('player-next').click();

	await expect(page.getByTestId('now-playing')).toContainText('Alpha Song');
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha', 'row-zulu']);
	await expect(announcement).toHaveText('');
});

test('refresh dropping a queued id clears the announcement and keeps survivors', async ({ page }) => {
	const payload: { current: unknown } = { current: ITEMS };
	await seedQueue(page, payload);
	const announcement = await moveAlphaUp(page);

	payload.current = [ITEMS[0], ITEMS[2]];
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();

	await expect(page.getByTestId('library-item')).toHaveCount(2);
	await expect.poll(() => queueOrder(page)).toEqual(['row-mike', 'row-zulu']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
	await expect(announcement).toHaveText('');
});

test('storage auth transition clears a move announcement before the next session retry', async ({ page }) => {
	const payload: { current: unknown } = { current: ITEMS };
	await seedQueue(page, payload);
	await moveAlphaUp(page);

	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate((key) => {
		window.localStorage.setItem(key, 'test-auth-transition');
		window.localStorage.removeItem(key);
	}, 'nntune.library.auth-transition.v1');
	await otherTab.close();

	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByText('Alpha Song', { exact: false })).toHaveCount(0);
	payload.current = [{ id: 'row-bravo', title: 'Bravo Song' }];
	await page.getByRole('button', { name: 'Retry', exact: true }).click();

	await expect(page.getByTestId('library-list')).toContainText('Bravo Song');
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('queue-announcement')).toHaveText('');
});

test('same-tab auth transition clears a move announcement before the next session retry', async ({ page }) => {
	// As in security-auth-transition.spec.ts, keep the real logout continuation
	// alive across client navigation. The transport ignores cancellation; all
	// security, local-dispatch, library and queue handlers remain real.
	await page.addInitScript(() => {
		const nativeFetch = window.fetch.bind(window);
		const held = { pending: false, release: () => {} };
		Object.assign(window, { __queueAnnouncementLogout: held });
		window.fetch = (input, init) => {
			const request = new Request(input, init);
			if (new URL(request.url).pathname !== '/v1/session/logout') {
				return nativeFetch(input, init);
			}
			held.pending = true;
			return new Promise<Response>((resolve) => {
				held.release = () => resolve(new Response(JSON.stringify({ ok: true }), {
					status: 200,
					headers: { 'content-type': 'application/json' }
				}));
			});
		};
	});
	await page.route('**/v1/**', (route) => route.fulfill({ status: 500, json: {} }));
	const payload: { current: unknown } = { current: ITEMS };
	await page.route('**/api/library', (route) => route.fulfill({
		status: 200, json: payload.current
	}));
	await page.goto('/account/security');
	await page.getByRole('button', { name: 'Sign out', exact: true }).click();
	await expect.poll(() => page.evaluate(() =>
		(window as Window & { __queueAnnouncementLogout: { pending: boolean } })
			.__queueAnnouncementLogout.pending
	)).toBe(true);

	await page.evaluate(() => {
		const link = document.createElement('a');
		link.href = '/library';
		link.textContent = 'Open library with logout pending';
		document.body.append(link);
	});
	await page.getByRole('link', { name: 'Open library with logout pending', exact: true }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(3);
	await row(page, 'row-mike').click();
	await row(page, 'row-zulu').click();
	await row(page, 'row-alpha').click();
	await moveAlphaUp(page);

	await page.evaluate(() =>
		(window as Window & { __queueAnnouncementLogout: { release: () => void } })
			.__queueAnnouncementLogout.release()
	);
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page).toHaveURL(/\/library$/);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByText('Alpha Song', { exact: false })).toHaveCount(0);
	payload.current = [{ id: 'row-bravo', title: 'Bravo Song' }];
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expect(page.getByTestId('library-list')).toContainText('Bravo Song');
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('queue-announcement')).toHaveText('');
});

test('remove replaces a stale move announcement with the remove outcome', async ({ page }) => {
	await seedQueue(page);
	const announcement = await moveAlphaUp(page);

	const remove = rowButton(page, 'row-zulu', 'Remove Zulu Song from queue');
	await expect(remove).toBeVisible();
	await remove.click();

	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha']);
	await expect(announcement).toHaveText('Zulu Song removed from the queue.');
	await expect(announcement).not.toContainText('moved to');
});

test('play-next clears a stale move announcement', async ({ page }) => {
	await seedQueue(page);
	const announcement = await moveAlphaUp(page);

	const playNext = rowButton(page, 'row-zulu', 'Play Zulu Song next');
	await expect(playNext).toBeVisible();
	await playNext.click();

	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu', 'row-alpha']);
	await expect(announcement).toHaveText('');
});

test('clear replaces the announcement with the cleared outcome', async ({ page }) => {
	await seedQueue(page);
	const announcement = await moveAlphaUp(page);

	const clear = page.getByRole('button', { name: 'Clear queue', exact: true });
	await expect(clear).toBeVisible();
	await clear.click();
	await page.getByRole('button', { name: 'Yes, clear queue', exact: true }).click();

	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(announcement).toHaveText('Queue cleared.');
	await expect(announcement).not.toContainText('moved to');
});
