import { expect, test, type Page } from '@playwright/test';

// Acceptance contract: account-security sign-out actions must invalidate
// library snapshots across tabs. A successful local logout, a successful
// revoke-all, and a confirmed local 401 all cross an auth boundary, so the
// acting tab's stored snapshot (session- and local-scoped copies) must be
// cleared and an already-open library tab must stop showing its cached
// library. A generic 500 or a transport failure confirms nothing about the
// session, so it must neither claim sign-out nor clear a still-authenticated
// account's state. A revoke-all 401 additionally keeps the explicit notice
// that other devices were NOT signed out and must not look like success.

const SNAPSHOT_KEY = 'nntune.library.snapshot.v1';
const AUTH_CONTEXT_KEY = 'nntune.library.auth-context.v1';
const AUTH_CONTEXT = 'test-account-a';

const SEEDED = {
	savedAt: '2026-10-15T11:00:00.000Z',
	items: [
		{ id: 'track-seed-1', title: 'Seeded Song One', artist: 'Seed Artist' },
		{ id: 'track-seed-2', title: 'Seeded Song Two' }
	]
};

interface StoredSnapshotState {
	context: string | null;
	scoped: string | null;
	legacy: string | null;
}

/** Seed the acting tab with a stored snapshot in both storage areas. */
async function seedActingTabSnapshot(page: Page): Promise<void> {
	await page.evaluate(
		({ key, contextKey, context, snapshot }) => {
			for (const storage of [window.sessionStorage, window.localStorage]) {
				storage.setItem(contextKey, context);
				storage.setItem(`${key}.${context}`, JSON.stringify(snapshot));
				storage.setItem(key, JSON.stringify(snapshot));
			}
		},
		{ key: SNAPSHOT_KEY, contextKey: AUTH_CONTEXT_KEY, context: AUTH_CONTEXT, snapshot: SEEDED }
	);
}

async function readStoredSnapshot(
	page: Page,
	area: 'sessionStorage' | 'localStorage'
): Promise<StoredSnapshotState> {
	return page.evaluate(
		({ key, contextKey, areaName }) => {
			const storage = window[areaName];
			const context = storage.getItem(contextKey);
			return {
				context,
				scoped: context ? storage.getItem(`${key}.${context}`) : null,
				legacy: storage.getItem(key)
			};
		},
		{ key: SNAPSHOT_KEY, contextKey: AUTH_CONTEXT_KEY, areaName: area }
	);
}

async function expectSnapshotCleared(page: Page): Promise<void> {
	for (const area of ['sessionStorage', 'localStorage'] as const) {
		const stored = await readStoredSnapshot(page, area);
		expect(stored.context, `${area} auth context survived sign-out`).toBeNull();
		expect(stored.scoped, `${area} scoped snapshot survived sign-out`).toBeNull();
		expect(stored.legacy, `${area} legacy snapshot survived sign-out`).toBeNull();
	}
}

async function expectSnapshotPresent(page: Page): Promise<void> {
	const stored = await readStoredSnapshot(page, 'sessionStorage');
	expect(stored.context, 'session auth context was cleared without sign-out').toBe(AUTH_CONTEXT);
	expect(stored.scoped, 'session snapshot was cleared without sign-out').not.toBeNull();
	expect(JSON.parse(stored.scoped as string).items.map((i: { id: string }) => i.id)).toEqual([
		'track-seed-1',
		'track-seed-2'
	]);
}

/** Open a library tab that has verified its session and cached the list. */
async function openLibraryTab(page: Page): Promise<void> {
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(SEEDED.items)
		})
	);
	await page.goto('/library');
	await expect(page.getByTestId('library-list')).toContainText('Seeded Song One');
}

async function expectLibraryTabInvalidated(page: Page): Promise<void> {
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('library-list')).toHaveCount(0);
	const stored = await readStoredSnapshot(page, 'sessionStorage');
	expect(stored.scoped, 'library tab snapshot survived the auth transition').toBeNull();
	expect(stored.context, 'library tab auth context survived the auth transition').toBeNull();
}

async function expectLibraryTabUntouched(page: Page): Promise<void> {
	await expect(page.getByTestId('library-list')).toContainText('Seeded Song One');
	await expect(page.getByRole('alert')).toHaveCount(0);
	const stored = await readStoredSnapshot(page, 'sessionStorage');
	expect(stored.scoped, 'library tab snapshot was cleared without sign-out').not.toBeNull();
}

/** Open the account-security tab with the session probe mocked reachable. */
async function openSecurityTab(libraryTab: Page): Promise<Page> {
	const securityTab = await libraryTab.context().newPage();
	await securityTab.route('**/v1/vault', (route) =>
		route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({}) })
	);
	return securityTab;
}

async function mockSessionEndpoint(
	page: Page,
	path: 'logout' | 'revoke-all',
	status: number
): Promise<() => number> {
	let posts = 0;
	await page.route(`**/v1/session/${path}`, async (route) => {
		posts += 1;
		await route.fulfill({
			status,
			contentType: 'application/json',
			body: JSON.stringify(status === 200 ? { ok: true } : { error: 'request failed' })
		});
	});
	return () => posts;
}

test('successful local logout invalidates library snapshots across tabs before redirect', async ({
	page
}) => {
	await openLibraryTab(page);
	const securityTab = await openSecurityTab(page);
	const logoutPosts = await mockSessionEndpoint(securityTab, 'logout', 200);

	await securityTab.goto('/account/security');
	await seedActingTabSnapshot(securityTab);
	await securityTab.getByRole('button', { name: /^sign out$/i }).click();

	await securityTab.waitForURL('**/login');
	expect(logoutPosts()).toBe(1);
	await expectSnapshotCleared(securityTab);
	await expectLibraryTabInvalidated(page);
	await securityTab.close();
});

test('successful revoke-all invalidates library snapshots across tabs before redirect', async ({
	page
}) => {
	await openLibraryTab(page);
	const securityTab = await openSecurityTab(page);
	const revokePosts = await mockSessionEndpoint(securityTab, 'revoke-all', 200);

	await securityTab.goto('/account/security');
	await seedActingTabSnapshot(securityTab);
	securityTab.once('dialog', (dialog) => dialog.accept());
	await securityTab.getByRole('button', { name: /sign out everywhere/i }).click();

	await securityTab.waitForURL('**/login');
	expect(revokePosts()).toBe(1);
	await expectSnapshotCleared(securityTab);
	await expectLibraryTabInvalidated(page);
	await securityTab.close();
});

test('logout confirmed 401 invalidates library snapshots across tabs before redirect', async ({
	page
}) => {
	await openLibraryTab(page);
	const securityTab = await openSecurityTab(page);
	const logoutPosts = await mockSessionEndpoint(securityTab, 'logout', 401);

	await securityTab.goto('/account/security');
	await seedActingTabSnapshot(securityTab);
	await securityTab.getByRole('button', { name: /^sign out$/i }).click();

	await securityTab.waitForURL('**/login');
	expect(logoutPosts()).toBe(1);
	await expectSnapshotCleared(securityTab);
	await expectLibraryTabInvalidated(page);
	await securityTab.close();
});

test('revoke-all 401 invalidates snapshots but keeps the not-revoked notice without redirect', async ({
	page
}) => {
	await openLibraryTab(page);
	const securityTab = await openSecurityTab(page);
	const revokePosts = await mockSessionEndpoint(securityTab, 'revoke-all', 401);

	await securityTab.goto('/account/security');
	await seedActingTabSnapshot(securityTab);
	securityTab.once('dialog', (dialog) => dialog.accept());
	await securityTab.getByRole('button', { name: /sign out everywhere/i }).click();

	// The local session is confirmed gone, so local snapshots are invalidated;
	// the revocation itself did not happen, so the page must stay put and keep
	// saying other devices were not signed out instead of looking like success.
	const alert = securityTab.getByRole('alert');
	await expect(alert).toBeVisible();
	await expect(alert).toContainText(/session expired before other devices were signed out/i);
	await expect(alert.getByRole('link', { name: /sign in again/i })).toHaveAttribute(
		'href',
		'/login'
	);
	expect(revokePosts()).toBe(1);
	expect(new URL(securityTab.url()).pathname).toBe('/account/security');
	await expectSnapshotCleared(securityTab);
	await expectLibraryTabInvalidated(page);
	await securityTab.close();
});

test('logout 500 does not claim sign-out or clear a still-authenticated account state', async ({
	page
}) => {
	await openLibraryTab(page);
	const securityTab = await openSecurityTab(page);
	const logoutPosts = await mockSessionEndpoint(securityTab, 'logout', 500);

	await securityTab.goto('/account/security');
	await seedActingTabSnapshot(securityTab);
	await securityTab.getByRole('button', { name: /^sign out$/i }).click();

	const alert = securityTab.getByRole('alert');
	await expect(alert).toBeVisible();
	await expect(alert).not.toContainText(/signed out|success/i);
	expect(logoutPosts()).toBe(1);
	expect(new URL(securityTab.url()).pathname).toBe('/account/security');
	await expectSnapshotPresent(securityTab);
	await expectLibraryTabUntouched(page);
	await securityTab.close();
});

test('revoke-all transport failure does not claim revocation or clear account state', async ({
	page
}) => {
	await openLibraryTab(page);
	const securityTab = await openSecurityTab(page);
	let revokePosts = 0;
	await securityTab.route('**/v1/session/revoke-all', async (route) => {
		revokePosts += 1;
		await route.abort('failed');
	});

	await securityTab.goto('/account/security');
	await seedActingTabSnapshot(securityTab);
	securityTab.once('dialog', (dialog) => dialog.accept());
	await securityTab.getByRole('button', { name: /sign out everywhere/i }).click();

	const alert = securityTab.getByRole('alert');
	await expect(alert).toBeVisible();
	await expect(alert).not.toContainText(/signed out|success/i);
	expect(revokePosts).toBe(1);
	expect(new URL(securityTab.url()).pathname).toBe('/account/security');
	await expectSnapshotPresent(securityTab);
	await expectLibraryTabUntouched(page);
	await securityTab.close();
});

test('blocked storage does not turn a successful logout into a failure', async ({ page }) => {
	await page.addInitScript(() => {
		for (const name of ['sessionStorage', 'localStorage']) {
			Object.defineProperty(window, name, {
				configurable: true,
				get() {
					throw new DOMException('The operation is insecure.', 'SecurityError');
				}
			});
		}
	});
	await page.route('**/v1/vault', (route) =>
		route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({}) })
	);
	const logoutPosts = await mockSessionEndpoint(page, 'logout', 200);

	await page.goto('/account/security');
	await page.getByRole('button', { name: /^sign out$/i }).click();

	// Privacy-restricted storage must not land a successful sign-out in the
	// error branch: the redirect to /login is the success path.
	await page.waitForURL('**/login');
	expect(logoutPosts()).toBe(1);
});
