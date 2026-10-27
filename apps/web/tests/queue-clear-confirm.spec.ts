import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Acceptance: destructive queue clears require explicit in-page confirmation.
 *
 * OWNERSHIP: this file is acceptance-owned and protected. The paired Builder
 * implements the confirm flows on top of this branch and must turn these
 * tests GREEN without editing this file; a wrong test comes back to the
 * acceptance card instead.
 *
 * Human-facing contract (no implementation/source-shape checks):
 * - "Clear queue" with a non-empty queue opens an IN-PAGE confirmation that
 *   discloses removal and irreversibility ("cannot be undone" / irreversible /
 *   permanently), with explicit Confirm and Cancel buttons. No window.confirm.
 *   The queue is untouched until a choice is made.
 * - Confirm wipes the whole queue into the empty state (now-playing reads
 *   "Queue is empty", player-next and "Clear queue" disabled), announces a
 *   concise cleared outcome through the polite queue-announcement region, and
 *   moves focus to the queue-empty-focus target (the existing queue-removal
 *   focus-recovery semantics: never the now-disabled Clear button, never
 *   the body). Reload-after-clear stays cleared through the restore channel.
 * - Cancel changes nothing (order, current track, now-playing, announcement,
 *   storage) and restores focus to the "Clear queue" trigger.
 * - "Clear upcoming" (trim-upcoming) carries the same confirm/cancel contract:
 *   the dialog discloses upcoming removal + irreversibility; Cancel preserves
 *   everything and refocuses its trigger; Confirm removes only upcoming rows,
 *   keeps the current track highlighted, announces the removed count with
 *   "removed" (never "played"), and focuses the surviving current row.
 * - Clear on an empty queue (or trim with nothing upcoming) opens no confirm
 *   and stays silent: triggers disabled, no dialog, no announcement, no focus
 *   move.
 * - Auth teardown while a confirm is open dismisses the dialog without acting;
 *   confirm state never survives unmount or an account change (Retry starts
 *   clean with no dialog).
 * - A prompt whose precondition disappears while the panel stays mounted
 *   (nothing left upcoming) dismisses itself and does not reappear when the
 *   precondition returns on that same mount.
 * - Focus recovery matches the existing queue-removal focus recovery
 *   (`library-queue-remove-focus.spec.ts`): cancel restores the trigger;
 *   confirmed whole-clear lands on queue-empty-focus; confirmed trim lands
 *   on the surviving current row. Keyboard: Tab reaches every control,
 *   Enter/Space activates, focus stays visible.
 * - No favorites or playback side effects: the favorites shelf and its scoped
 *   storage are untouched, no media play/pause/load/seek/src writes, no audio
 *   element appears, and no request beyond GET /api/library is issued.
 *
 * Out of scope (not pinned): undo/redo, favorites-clear changes, keep-current
 * confirmation, the queue restore PR itself, backend work, drag-and-drop,
 * audio transport and stream URLs.
 *
 * Harness note (same as the neighbouring library specs): `/library` is
 * client-fetched (`ssr = false`), so the click-driven contract is pinned here
 * with a route-mocked `/api/library`. Every test guards its first
 * confirmation interaction with an explicit visibility assertion, so a run
 * against a build without the confirm fails as an assertion (missing dialog),
 * never as a click error. RED here means the confirmation is missing: the
 * first click already wipes or trims while the dialog stays absent.
 *
 * From apps/web (Node 24 / pnpm 10):
 *   pnpm install --frozen-lockfile
 *   pnpm exec playwright install chromium
 *   CI=1 PUBLIC_API_BASE_URL='' pnpm exec playwright test tests/queue-clear-confirm.spec.ts --workers=1
 *   pnpm check
 *   pnpm test
 * Regressions: same test:e2e command with tests/library-queue-manage.spec.ts,
 * tests/library-queue-trim.spec.ts, tests/library-queue-remove-focus.spec.ts
 * and tests/library-favorites-management.spec.ts.
 * The configured harness builds this checkout and refuses preview reuse in CI.
 * If 4173 is busy, use a run-owned config extending the repository config with
 * a free port/baseURL and reuseExistingServer:false; do not stop/reuse a peer.
 *
 * Expected pre-feature RED: QC-B01..B04 and QC-B06..B10 fail on dialog or
 * preservation assertions (queue already wiped/trimmed, confirm/cancel absent);
 * QC-B00 and QC-B05 pass as controls. Builder next action: implement the
 * in-page confirms in the shared queue component plus host clear/trim
 * announcements, then run this file plus the protected regressions and
 * same-head PR gates. No existing test or production file is changed here.
 */

// Rendered order is API order; ids are deliberately not in sorted order.
const ITEMS = [
	{ id: 'row-zulu', title: 'Zulu Song', artist: 'Queue Artist' },
	{ id: 'row-alpha', title: 'Alpha Song' },
	{ id: 'row-mike', title: 'Mike Song', artist: 'Queue Artist' }
];
const TRANSITION = 'nntune.library.auth-transition.v1';
const FAVORITES_BASE = 'nntune.library.favorites.v1';

type Probe = { media: string[]; positions: number[]; sources: string[] };
type Harness = { requests: string[]; errors: string[] };

function row(page: Page, id: string): Locator {
	return page.locator(`[data-testid="library-item"][data-id="${id}"]`);
}

function queueItem(page: Page, id: string): Locator {
	return page.locator(
		`[data-testid="queue-item"][data-track-id="${id}"], [data-testid="queue-item"][data-id="${id}"]`
	);
}

function clearTrigger(page: Page): Locator {
	return page.getByRole('button', { name: 'Clear queue', exact: true });
}

function trimTrigger(page: Page): Locator {
	return page.getByRole('button', { name: 'Clear upcoming', exact: true });
}

function confirmButton(page: Page): Locator {
	// Anchored alternatives avoid the "Clear queue" / "Clear upcoming" triggers:
	// those start with "Clear " plus a word, never "Confirm", "Yes", or a count.
	return page.getByRole('button', { name: /^(confirm|yes\b|clear\s+\d+)/i });
}

function cancelButton(page: Page): Locator {
	// The lookahead excludes the "Keep current and upcoming" trim trigger while
	// still matching "Keep", "Keep queue", "Cancel", "Don't clear", and friends.
	return page.getByRole('button', {
		name: /^(cancel|don't clear|do not clear|keep(?!\s+current\b))/i
	});
}

const IRREVERSIBLE = /cannot be undone|irreversible|permanently|no undo/i;

function clearDialog(page: Page): Locator {
	// Any container holding BOTH the irreversibility disclosure and removal
	// language. The triggers ("Clear queue") never carry the former, so this
	// cannot match a trigger; no dialog role is required of the Builder.
	return page
		.locator('div, section, dialog, [role="dialog"], [role="alertdialog"]')
		.filter({ hasText: IRREVERSIBLE })
		.filter({ hasText: /remov|clear|delet|wip/i });
}

function trimDialog(page: Page): Locator {
	return page
		.locator('div, section, dialog, [role="dialog"], [role="alertdialog"]')
		.filter({ hasText: IRREVERSIBLE })
		.filter({ hasText: /upcoming/i });
}

function announcement(page: Page): Locator {
	return page.getByTestId('queue-announcement');
}

function confirmFeedback(page: Page): Locator {
	return page.getByTestId('queue-confirm-feedback');
}

function emptyTarget(page: Page): Locator {
	return page.getByTestId('queue-empty-focus');
}

function currentRow(page: Page): Locator {
	return page.locator('[data-testid="queue-item"][aria-current="true"]');
}

function star(page: Page, id: string): Locator {
	return row(page, id).getByRole('button', { name: /star|favorite/i });
}

function shelfIds(page: Page): Promise<(string | null)[]> {
	return page
		.getByTestId('favorite-item')
		.evaluateAll((items) => items.map((item) => item.getAttribute('data-id')));
}

async function queueOrder(page: Page): Promise<(string | null)[]> {
	return page
		.getByTestId('queue-item')
		.evaluateAll((items) =>
			items.map((item) => item.getAttribute('data-track-id') ?? item.getAttribute('data-id'))
		);
}

async function favoritesStorage(page: Page): Promise<Record<string, string | null>> {
	return page.evaluate((base) => {
		const snapshot: Record<string, string | null> = {};
		for (let index = 0; index < localStorage.length; index += 1) {
			const key = localStorage.key(index);
			if (key?.startsWith(`${base}.`) ?? false) snapshot[key!] = localStorage.getItem(key!);
		}
		return snapshot;
	}, FAVORITES_BASE);
}

async function probeState(page: Page): Promise<Probe> {
	return page.evaluate(() => (window as unknown as { queueClearProbe: Probe }).queueClearProbe);
}

async function settle(page: Page): Promise<void> {
	await page.evaluate(
		() =>
			new Promise<void>((resolve) =>
				requestAnimationFrame(() => requestAnimationFrame(() => resolve()))
			)
	);
}

async function seedLibrary(
	page: Page,
	payload: { current: unknown } = { current: ITEMS }
): Promise<Harness> {
	const requests: string[] = [];
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(error.message));
	page.on('request', (request) => {
		const url = new URL(request.url());
		if (
			['fetch', 'xhr', 'media'].includes(request.resourceType()) ||
			url.pathname.startsWith('/api/') ||
			url.pathname.startsWith('/v1/') ||
			request.method() !== 'GET'
		) {
			requests.push(`${request.method()} ${url.pathname}${url.search}`);
		}
	});
	await page.addInitScript(() => {
		const probe: Probe = { media: [], positions: [], sources: [] };
		Object.assign(window, { queueClearProbe: probe });
		for (const method of ['play', 'pause', 'load'] as const) {
			Object.defineProperty(HTMLMediaElement.prototype, method, {
				configurable: true,
				value() {
					probe.media.push(method);
					return method === 'play' ? Promise.resolve() : undefined;
				}
			});
		}
		for (const property of ['currentTime', 'src'] as const) {
			const descriptor = Object.getOwnPropertyDescriptor(HTMLMediaElement.prototype, property)!;
			Object.defineProperty(HTMLMediaElement.prototype, property, {
				...descriptor,
				set(value: number | string) {
					if (property === 'currentTime') probe.positions.push(Number(value));
					else probe.sources.push(String(value));
					descriptor.set!.call(this, value);
				}
			});
		}
	});
	await page.route('**/api/library', (route) =>
		route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(payload.current) })
	);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	return { requests, errors };
}

/** Fill the queue in gesture order: mike, zulu, alpha. Current is mike. */
async function seedQueue(page: Page, payload?: { current: unknown }): Promise<Harness> {
	const harness = await seedLibrary(page, payload);
	await row(page, 'row-mike').click();
	await row(page, 'row-zulu').click();
	await row(page, 'row-alpha').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
	return harness;
}

async function expectNoPlayback(page: Page, harness: Harness): Promise<void> {
	await settle(page);
	expect(await probeState(page)).toEqual({ media: [], positions: [], sources: [] });
	await expect(page.locator('audio')).toHaveCount(0);
	expect(harness.requests).toEqual(['GET /api/library']);
	expect(harness.errors).toEqual([]);
}

async function tabTo(page: Page, target: Locator): Promise<void> {
	await expect(target).toBeVisible();
	for (let tabs = 0; tabs < 80; tabs += 1) {
		if (await target.evaluate((element) => element === document.activeElement)) break;
		await page.keyboard.press('Tab');
	}
	await expect(target, 'control must be reachable by Tab').toBeFocused();
	const ring = await target.evaluate((element) => {
		const style = getComputedStyle(element);
		return (
			element.matches(':focus-visible') &&
			((style.outlineStyle !== 'none' && parseFloat(style.outlineWidth) > 0) ||
				style.boxShadow !== 'none')
		);
	});
	expect(ring, 'keyboard focus needs a visible outline or focus ring').toBe(true);
}

test('QC-B00 control: seeded queue exposes enabled clear triggers with no dialog pending', async ({
	page
}) => {
	const harness = await seedQueue(page);
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu', 'row-alpha']);
	await expect(currentRow(page)).toHaveAttribute('data-track-id', 'row-mike');
	await expect(page.getByTestId('player-next')).toBeEnabled();
	await expect(clearTrigger(page)).toBeVisible();
	await expect(clearTrigger(page)).toBeEnabled();
	await expect(trimTrigger(page)).toBeVisible();
	await expect(trimTrigger(page)).toBeEnabled();
	await expect(confirmButton(page)).toHaveCount(0);
	await expect(cancelButton(page)).toHaveCount(0);
	await expect(announcement(page)).toHaveText('');
	await expectNoPlayback(page, harness);
});

test('QC-B01 Clear opens an explicit in-page confirm disclosing removal and irreversibility', async ({
	page
}) => {
	await seedQueue(page);
	let dialogs = 0;
	page.on('dialog', async (dialog) => {
		dialogs += 1;
		await dialog.dismiss();
	});

	const clear = clearTrigger(page);
	await expect(clear).toBeVisible();
	await expect(clear).toBeEnabled();
	await clear.click();

	// The arrangement survives until an explicit choice; nothing is wiped early.
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu', 'row-alpha']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);

	await expect(confirmButton(page), 'clear requires an explicit in-page confirmation').toBeVisible();
	await expect(cancelButton(page), 'clear requires explicit in-page cancellation').toBeVisible();
	await expect(
		clearDialog(page),
		'confirm must disclose removal and irreversibility together'
	).toBeVisible();
	await expect(
		page.getByText('Remove all 3 tracks from the queue? This cannot be undone.', { exact: true })
	).toBeVisible();
	expect(dialogs, 'native dialogs are not an in-page confirmation').toBe(0);
});

test('QC-B02 Confirm wipes the queue, announces once, focuses the empty target, and stays cleared on reload', async ({
	page
}) => {
	const harness = await seedQueue(page);
	const clear = clearTrigger(page);
	await expect(clear).toBeVisible();
	await clear.click();
	await expect(confirmButton(page)).toBeVisible();
	await expect(cancelButton(page)).toBeVisible();

	await confirmButton(page).click();

	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
	await expect(page.getByTestId('player-next')).toBeDisabled();
	await expect(clearTrigger(page)).toBeDisabled();
	await expect(announcement(page)).toHaveAttribute('role', 'status');
	await expect(announcement(page)).not.toHaveAttribute('aria-live', 'assertive');
	await expect(announcement(page)).toContainText(/cleared/i);
	expect((await announcement(page).textContent())!.length).toBeLessThanOrEqual(160);

	// Like queue removal: focus lands on the empty target, never the
	// disabled trigger or body.
	await expect(emptyTarget(page)).toBeFocused();
	await expect(emptyTarget(page)).toHaveAttribute('tabindex', '-1');
	expect(await emptyTarget(page).evaluate((element) => element.tagName)).not.toBe('BUTTON');
	await expect(confirmButton(page)).toHaveCount(0);
	await expect(cancelButton(page)).toHaveCount(0);
	await expectNoPlayback(page, harness);

	// The restore channel keeps the wipe: reload stays cleared, never resurrected.
	await page.reload();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
	await expect(confirmButton(page)).toHaveCount(0);
	await expect(cancelButton(page)).toHaveCount(0);
});

test('QC-B03 Cancel changes nothing and restores focus to the Clear trigger', async ({ page }) => {
	const harness = await seedQueue(page);
	const favoritesBefore = await shelfIds(page);
	const storageBefore = await favoritesStorage(page);
	const clear = clearTrigger(page);
	await expect(clear).toBeVisible();
	await clear.click();
	await expect(cancelButton(page)).toBeVisible();
	await expect(confirmButton(page)).toBeVisible();

	await cancelButton(page).click();

	await expect(confirmFeedback(page)).toHaveText('Clear queue cancelled. Queue kept.');
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu', 'row-alpha']);
	await expect(currentRow(page)).toHaveAttribute('data-track-id', 'row-mike');
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
	await expect(page.getByTestId('player-next')).toBeEnabled();
	await expect(clearTrigger(page)).toBeEnabled();
	await expect(announcement(page)).toHaveText('');
	await expect(confirmButton(page)).toHaveCount(0);
	await expect(cancelButton(page)).toHaveCount(0);
	await expect(clearTrigger(page)).toBeFocused();
	expect(await shelfIds(page)).toEqual(favoritesBefore);
	expect(await favoritesStorage(page)).toEqual(storageBefore);
	await expectNoPlayback(page, harness);

	// The trigger stays usable: a second open still discloses before acting,
	// and reopening the prompt clears the earlier cancel note.
	await clearTrigger(page).click();
	await expect(confirmButton(page)).toBeVisible();
	await expect(confirmFeedback(page)).toHaveText('');
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu', 'row-alpha']);

	// Confirming after that reopen also leaves no stale cancel note behind.
	await confirmButton(page).click();
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(confirmFeedback(page)).toHaveText('');
});

test('QC-B04 trim-upcoming carries the same confirm/cancel contract and keeps the current track', async ({
	page
}) => {
	const harness = await seedQueue(page);
	await page.getByTestId('player-next').click();
	await expect(currentRow(page)).toHaveAttribute('data-track-id', 'row-zulu');
	const trim = trimTrigger(page);
	await expect(trim).toBeVisible();
	await expect(trim).toBeEnabled();
	await trim.click();

	// Cancel path: nothing changes, focus returns to the trim trigger.
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu', 'row-alpha']);
	await expect(confirmButton(page), 'trim requires an explicit in-page confirmation').toBeVisible();
	await expect(cancelButton(page), 'trim requires explicit in-page cancellation').toBeVisible();
	await expect(
		trimDialog(page),
		'trim confirm must disclose upcoming removal and irreversibility together'
	).toBeVisible();
	await expect(
		page.getByText(
			'Remove 1 upcoming track from the queue? The current track stays. This cannot be undone.',
			{ exact: true }
		)
	).toBeVisible();
	await cancelButton(page).click();
	await expect(confirmFeedback(page)).toHaveText('Clear upcoming cancelled. Queue kept.');
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu', 'row-alpha']);
	await expect(currentRow(page)).toHaveAttribute('data-track-id', 'row-zulu');
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');
	await expect(trimTrigger(page)).toBeFocused();

	// Confirm path: only upcoming rows go; current stays highlighted and focused.
	// Reopening the prompt clears the trim cancel note before the choice.
	await trimTrigger(page).click();
	await expect(confirmButton(page)).toBeVisible();
	await expect(confirmFeedback(page)).toHaveText('');
	await confirmButton(page).click();
	await expect.poll(() => queueOrder(page)).toEqual(['row-mike', 'row-zulu']);
	await expect(currentRow(page)).toHaveAttribute('data-track-id', 'row-zulu');
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');
	await expect(announcement(page)).toHaveAttribute('role', 'status');
	await expect(announcement(page)).not.toHaveAttribute('aria-live', 'assertive');
	await expect(announcement(page)).toContainText(/removed/i);
	await expect(announcement(page)).toContainText(/\b1\b/);
	await expect(announcement(page)).not.toContainText(/played/i);
	expect((await announcement(page).textContent())!.length).toBeLessThanOrEqual(160);
	await expect(currentRow(page)).toBeFocused();
	await expect(trimTrigger(page)).toBeDisabled();
	await expect(confirmButton(page)).toHaveCount(0);
	await expect(cancelButton(page)).toHaveCount(0);
	await expect(confirmFeedback(page)).toHaveText('');
	await expectNoPlayback(page, harness);

	// The trimmed arrangement survives a same-id background refresh.
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu']);
	await expect(currentRow(page)).toHaveAttribute('data-track-id', 'row-zulu');
});

test('QC-B05 empty queue and nothing-upcoming open no confirm and stay silent', async ({ page }) => {
	await seedLibrary(page);
	const clear = clearTrigger(page);
	await expect(clear).toBeVisible();
	await expect(clear).toBeDisabled();
	await expect(trimTrigger(page)).toBeDisabled();
	await expect(announcement(page)).toHaveText('');

	// Disabled activation cannot summon a dialog or disturb the empty state.
	await clear.evaluate((element) => (element as HTMLButtonElement).click());
	await trimTrigger(page).evaluate((element) => (element as HTMLButtonElement).click());
	await settle(page);
	await expect(confirmButton(page)).toHaveCount(0);
	await expect(cancelButton(page)).toHaveCount(0);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
	await expect(announcement(page)).toHaveText('');

	// A full queue trimmed to its tail also leaves trim silent afterwards.
	await row(page, 'row-mike').click();
	await row(page, 'row-zulu').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	await page.getByTestId('player-next').click();
	await expect(currentRow(page)).toHaveAttribute('data-track-id', 'row-zulu');
	await expect(trimTrigger(page)).toBeDisabled();
	await trimTrigger(page).evaluate((element) => (element as HTMLButtonElement).click());
	await settle(page);
	await expect(confirmButton(page)).toHaveCount(0);
	await expect(cancelButton(page)).toHaveCount(0);
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu']);
});

test('QC-B06 auth teardown dismisses an open confirm without acting; Retry starts clean', async ({
	page
}) => {
	const payload: { current: unknown } = { current: ITEMS };
	await seedQueue(page, payload);
	const clear = clearTrigger(page);
	await expect(clear).toBeVisible();
	await clear.click();
	await expect(confirmButton(page), 'confirm must be open before the auth transition').toBeVisible();
	await expect(cancelButton(page)).toBeVisible();
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu', 'row-alpha']);

	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate((key) => {
		localStorage.setItem(key, 'queue-clear-auth-transition');
		localStorage.removeItem(key);
	}, TRANSITION);
	await otherTab.close();

	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(confirmButton(page)).toHaveCount(0);
	await expect(cancelButton(page)).toHaveCount(0);
	await expect(announcement(page)).toHaveCount(0);

	// The next account retries into a clean queue with no resurrected dialog.
	payload.current = [{ id: 'row-bravo', title: 'Bravo Song' }];
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expect(page.getByTestId('library-list')).toContainText('Bravo Song');
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
	await expect(confirmButton(page)).toHaveCount(0);
	await expect(cancelButton(page)).toHaveCount(0);
	await expect(clearTrigger(page)).toBeDisabled();
});

test('QC-B07 confirm state never survives unmount or an account change', async ({ page }) => {
	await seedQueue(page);
	await clearTrigger(page).click();
	await expect(confirmButton(page), 'confirm must be open before unmount').toBeVisible();
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu', 'row-alpha']);

	// Unmount via reload and return: no dialog may survive the remount.
	await page.reload();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(confirmButton(page)).toHaveCount(0);
	await expect(cancelButton(page)).toHaveCount(0);
	await expect(announcement(page)).toHaveText('');

	// Re-seed, open the trim confirm, then change accounts: the dialog is gone.
	await row(page, 'row-mike').click();
	await row(page, 'row-zulu').click();
	await row(page, 'row-alpha').click();
	await expect(trimTrigger(page)).toBeEnabled();
	await trimTrigger(page).click();
	await expect(confirmButton(page)).toBeVisible();
	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate((key) => {
		localStorage.setItem(key, 'queue-clear-account-change');
		localStorage.removeItem(key);
	}, TRANSITION);
	await otherTab.close();
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(confirmButton(page)).toHaveCount(0);
	await expect(cancelButton(page)).toHaveCount(0);
});

test('QC-B08 keyboard flows reach every confirm control and recover focus visibly', async ({
	page
}) => {
	await seedQueue(page);
	const clear = clearTrigger(page);
	await tabTo(page, clear);
	await page.keyboard.press('Enter');
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu', 'row-alpha']);
	await expect(confirmButton(page)).toBeVisible();
	await expect(cancelButton(page)).toBeVisible();

	await tabTo(page, cancelButton(page));
	await page.keyboard.press('Space');
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu', 'row-alpha']);
	await expect(clearTrigger(page)).toBeFocused();

	await page.keyboard.press('Enter');
	await expect(confirmButton(page)).toBeVisible();
	await tabTo(page, confirmButton(page));
	await page.keyboard.press('Enter');
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(emptyTarget(page)).toBeFocused();

	// Trim keyboard path: Enter opens, Space on Cancel restores the trim trigger.
	await row(page, 'row-mike').click();
	await row(page, 'row-zulu').click();
	await row(page, 'row-alpha').click();
	await expect(trimTrigger(page)).toBeEnabled();
	await tabTo(page, trimTrigger(page));
	await page.keyboard.press('Enter');
	await expect(confirmButton(page)).toBeVisible();
	await tabTo(page, cancelButton(page));
	await page.keyboard.press('Space');
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu', 'row-alpha']);
	await expect(trimTrigger(page)).toBeFocused();
});

test('QC-B09 confirm flows leave favorites and playback untouched', async ({ page }) => {
	const harness = await seedQueue(page);
	await star(page, 'row-mike').click();
	await star(page, 'row-alpha').click();
	await expect.poll(() => shelfIds(page)).toEqual(['row-mike', 'row-alpha']);
	const storageBefore = await favoritesStorage(page);
	const shelfBefore = await shelfIds(page);

	// Cancel path: favorites shelf, scoped storage, and media stay identical.
	await clearTrigger(page).click();
	await expect(cancelButton(page)).toBeVisible();
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu', 'row-alpha']);
	await cancelButton(page).click();
	expect(await shelfIds(page)).toEqual(shelfBefore);
	expect(await favoritesStorage(page)).toEqual(storageBefore);
	await expectNoPlayback(page, harness);

	// Trim-cancel path: same shelf/storage/media invariance.
	await trimTrigger(page).click();
	await expect(cancelButton(page)).toBeVisible();
	await cancelButton(page).click();
	expect(await shelfIds(page)).toEqual(shelfBefore);
	expect(await favoritesStorage(page)).toEqual(storageBefore);

	// Confirm-clear path: the queue empties while favorites and media stay put.
	await clearTrigger(page).click();
	await expect(confirmButton(page)).toBeVisible();
	await confirmButton(page).click();
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	expect(await shelfIds(page)).toEqual(shelfBefore);
	expect(await favoritesStorage(page)).toEqual(storageBefore);
	await expectNoPlayback(page, harness);
});

test('QC-B10 a trim prompt invalidated while open dismisses itself and stays dismissed when upcoming returns', async ({
	page
}) => {
	await seedQueue(page);
	await trimTrigger(page).click();
	await expect(confirmButton(page), 'trim prompt must be open before invalidation').toBeVisible();
	await expect(cancelButton(page)).toBeVisible();

	// Advance the current track to the tail on the same mounted panel: with
	// nothing left upcoming the open prompt's precondition is gone.
	await page.getByTestId('player-next').click();
	await expect(currentRow(page)).toHaveAttribute('data-track-id', 'row-zulu');
	await expect(confirmButton(page)).toBeVisible();
	await page.getByTestId('player-next').click();
	await expect(currentRow(page)).toHaveAttribute('data-track-id', 'row-alpha');
	await expect(trimTrigger(page)).toBeDisabled();
	await expect(confirmButton(page)).toHaveCount(0);
	await expect(cancelButton(page)).toHaveCount(0);

	// Restore the precondition by selecting an earlier row: the dismissed
	// prompt must not resurrect, and nothing was trimmed behind it.
	await queueItem(page, 'row-mike')
		.getByRole('button', { name: 'Select Mike Song', exact: true })
		.click();
	await expect(currentRow(page)).toHaveAttribute('data-track-id', 'row-mike');
	await expect(trimTrigger(page)).toBeEnabled();
	await settle(page);
	await expect(confirmButton(page)).toHaveCount(0);
	await expect(cancelButton(page)).toHaveCount(0);
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu', 'row-alpha']);
});
