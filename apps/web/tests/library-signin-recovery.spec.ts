import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Acceptance: the library error state offers typed sign-in recovery.
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build lands the
 * implementation on top of this branch and must make these tests pass
 * WITHOUT editing this file; a wrong test comes back to the acceptance
 * card instead. The refresher state machine behind this surface is
 * pinned in `src/lib/library/refresh.signin-recovery.test.ts`.
 *
 * Contract under test (paired build):
 * - After a 401 (initial or following a successful load) or a cross-tab
 *   auth transition, the error state shows a real anchor whose accessible
 *   name matches /sign in/i and whose `href` is the existing `/login`
 *   route. It is keyboard-activatable: focus + Enter navigates to the
 *   login page. (There is no other sign-in link on `/library`, so the
 *   role/name locator is unique.)
 * - 403, network/timeout failures, and 404 never show that link: 403
 *   wording stays neutral about the session, unreachable failures keep
 *   the Retry button (and Retry still recovers), and 404 keeps the
 *   neutral not-yet-available state.
 * - Privacy behavior is unchanged throughout: a prior account's tracks
 *   and queue never reappear during the failure, on the way to `/login`,
 *   or after the next account's library loads.
 */

const SNAPSHOT_KEY = 'nntune.library.snapshot.v1';
const AUTH_CONTEXT_KEY = 'nntune.library.auth-context.v1';

const MINE = [
	{ id: 'track-mine-1', title: 'Private Song One', artist: 'First Account' },
	{ id: 'track-mine-2', title: 'Private Song Two' }
];

const NEXT_ACCOUNT = [{ id: 'track-next-1', title: 'Next Account Song' }];

type LibraryBehavior =
	| { kind: 'ok'; items: unknown }
	| { kind: 'status'; status: number }
	| { kind: 'abort' }
	| { kind: 'hang' };

/** Route `/api/library` to a mutable behavior so a test can fail mid-run. */
async function routeLibrary(page: Page, behavior: { current: LibraryBehavior }): Promise<void> {
	await page.route('**/api/library', async (route) => {
		const current = behavior.current;
		if (current.kind === 'abort') return route.abort('failed');
		if (current.kind === 'hang') return new Promise(() => {});
		if (current.kind === 'status') {
			return route.fulfill({
				status: current.status,
				contentType: 'application/json',
				body: JSON.stringify({ error: 'denied' })
			});
		}
		return route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(current.items)
		});
	});
}

async function seedSnapshot(page: Page): Promise<void> {
	await page.addInitScript(
		({ key, contextKey, items }) => {
			const context = 'test-account-a';
			window.sessionStorage.setItem(contextKey, context);
			window.sessionStorage.setItem(
				`${key}.${context}`,
				JSON.stringify({ savedAt: '2026-10-15T11:00:00.000Z', items })
			);
		},
		{ key: SNAPSHOT_KEY, contextKey: AUTH_CONTEXT_KEY, items: MINE }
	);
}

async function readSnapshot(page: Page): Promise<string | null> {
	return page.evaluate(
		({ key, contextKey }) => {
			const context = window.sessionStorage.getItem(contextKey);
			return context ? window.sessionStorage.getItem(`${key}.${context}`) : null;
		},
		{ key: SNAPSHOT_KEY, contextKey: AUTH_CONTEXT_KEY }
	);
}

function signInLink(page: Page): Locator {
	return page.getByRole('link', { name: /sign in/i });
}

function libraryRow(page: Page, id: string): Locator {
	return page.locator(`[data-testid="library-item"][data-id="${id}"] button`);
}

async function expectNoPrivateTracks(page: Page): Promise<void> {
	await expect(page.getByTestId('library-list')).toHaveCount(0);
	await expect(page.getByText('Private Song One')).toHaveCount(0);
	await expect(page.getByText('Private Song Two')).toHaveCount(0);
}

async function fireAuthTransition(page: Page): Promise<void> {
	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate((key) => {
		window.localStorage.setItem(key, 'test-auth-transition');
		window.localStorage.removeItem(key);
	}, 'nntune.library.auth-transition.v1');
	await otherTab.close();
}

test('initial 401 offers a keyboard-accessible sign-in link and hides the seeded snapshot', async ({
	page
}) => {
	await seedSnapshot(page);
	const behavior: { current: LibraryBehavior } = { current: { kind: 'status', status: 401 } };
	await routeLibrary(page, behavior);

	await page.goto('/library');

	await expect(page.getByRole('alert')).toBeVisible();
	await expectNoPrivateTracks(page);
	expect(await readSnapshot(page)).toBeNull();

	const link = signInLink(page);
	await expect(link).toBeVisible();
	await expect(link).toHaveAttribute('href', '/login');

	// Keyboard activation: Tab-focusable native anchor, Enter navigates.
	await link.focus();
	await expect(link).toBeFocused();
	await page.keyboard.press('Enter');
	await page.waitForURL('**/login');
	await expect(page.getByRole('heading', { name: 'Sign in' })).toBeVisible();
});

test('success then 401 offers sign-in, and the prior queue never reaches the next account', async ({
	page
}) => {
	const behavior: { current: LibraryBehavior } = { current: { kind: 'ok', items: MINE } };
	await routeLibrary(page, behavior);

	await page.goto('/library');
	await expect(page.getByTestId('library-list')).toContainText('Private Song One');
	await libraryRow(page, 'track-mine-1').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	await expect(page.getByTestId('now-playing')).toContainText('Private Song One');

	behavior.current = { kind: 'status', status: 401 };
	await page.getByRole('button', { name: 'Refresh' }).click();

	await expect(page.getByRole('alert')).toBeVisible();
	await expect(signInLink(page)).toHaveAttribute('href', '/login');
	await expectNoPrivateTracks(page);
	expect(await readSnapshot(page)).toBeNull();

	// The next account signs in (simulated by the endpoint recovering):
	// its library loads, but the previous account's queue stays gone.
	behavior.current = { kind: 'ok', items: NEXT_ACCOUNT };
	await page.getByRole('button', { name: 'Retry' }).click();

	await expect(page.getByTestId('library-list')).toContainText('Next Account Song');
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
	await expect(signInLink(page)).toHaveCount(0);
});

test('cross-tab auth transition offers sign-in and private tracks stay gone on the way there', async ({
	page
}) => {
	const behavior: { current: LibraryBehavior } = { current: { kind: 'ok', items: MINE } };
	await routeLibrary(page, behavior);

	await page.goto('/library');
	await expect(page.getByTestId('library-list')).toContainText('Private Song One');
	await libraryRow(page, 'track-mine-2').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);

	await fireAuthTransition(page);

	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expectNoPrivateTracks(page);
	expect(await readSnapshot(page)).toBeNull();

	const link = signInLink(page);
	await expect(link).toBeVisible();
	await expect(link).toHaveAttribute('href', '/login');
	await link.focus();
	await page.keyboard.press('Enter');
	await page.waitForURL('**/login');
	await expect(page.getByText('Private Song One')).toHaveCount(0);
});

test('403 stays session-neutral: private state cleared, Retry kept, no sign-in link', async ({
	page
}) => {
	await seedSnapshot(page);
	const behavior: { current: LibraryBehavior } = { current: { kind: 'status', status: 403 } };
	await routeLibrary(page, behavior);

	await page.goto('/library');

	const alert = page.getByRole('alert');
	await expect(alert).toBeVisible();
	// A forbidden response does not prove the session ended.
	await expect(alert).not.toContainText(/signed out|sign in|expired/i);
	await expect(page.getByRole('button', { name: 'Retry' })).toBeVisible();
	await expect(signInLink(page)).toHaveCount(0);
	await expectNoPrivateTracks(page);
	expect(await readSnapshot(page)).toBeNull();
});

test('network failure keeps Retry with no sign-in link, and Retry recovers', async ({ page }) => {
	const behavior: { current: LibraryBehavior } = { current: { kind: 'abort' } };
	await routeLibrary(page, behavior);

	await page.goto('/library');

	await expect(page.getByRole('alert')).toContainText(/not reachable/i);
	await expect(page.getByRole('button', { name: 'Retry' })).toBeVisible();
	await expect(signInLink(page)).toHaveCount(0);

	behavior.current = { kind: 'ok', items: MINE };
	await page.getByRole('button', { name: 'Retry' }).click();

	await expect(page.getByTestId('library-list')).toContainText('Private Song One');
	await expect(page.getByRole('alert')).toHaveCount(0);
	await expect(signInLink(page)).toHaveCount(0);
});

test('a never-settling request times out into Retry with no sign-in link', async ({ page }) => {
	const behavior: { current: LibraryBehavior } = { current: { kind: 'hang' } };
	await routeLibrary(page, behavior);

	await page.goto('/library');

	await expect(page.getByRole('alert')).toContainText(/not reachable/i, { timeout: 12_000 });
	await expect(page.getByRole('button', { name: 'Retry' })).toBeVisible();
	await expect(signInLink(page)).toHaveCount(0);
});

test('404 keeps the neutral not-yet-available state with no sign-in link', async ({ page }) => {
	await seedSnapshot(page);
	const behavior: { current: LibraryBehavior } = { current: { kind: 'status', status: 404 } };
	await routeLibrary(page, behavior);

	await page.goto('/library');

	await expect(page.getByRole('status')).toContainText(/listing is not available yet/i);
	await expect(page.getByRole('alert')).toHaveCount(0);
	await expect(signInLink(page)).toHaveCount(0);
	await expect(page.getByRole('link', { name: 'Import an NZB' })).toHaveAttribute(
		'href',
		'/library/import'
	);
	await expectNoPrivateTracks(page);
});
