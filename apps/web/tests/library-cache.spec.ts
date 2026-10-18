import { expect, test } from '@playwright/test';

const SNAPSHOT_KEY = 'nntune.library.snapshot.v1';
const AUTH_CONTEXT_KEY = 'nntune.library.auth-context.v1';

const SEEDED = {
	savedAt: '2026-10-15T11:00:00.000Z',
	items: [
		{ id: 'track-seed-1', title: 'Seeded Song One', artist: 'Seed Artist' },
		{ id: 'track-seed-2', title: 'Seeded Song Two' }
	]
};

async function seedSnapshot(page: import('@playwright/test').Page): Promise<void> {
	await page.addInitScript(
		({ key, contextKey, snapshot }) => {
			const context = 'test-account-a';
			window.sessionStorage.setItem(contextKey, context);
			window.sessionStorage.setItem(`${key}.${context}`, JSON.stringify(snapshot));
		},
		{ key: SNAPSHOT_KEY, contextKey: AUTH_CONTEXT_KEY, snapshot: SEEDED }
	);
}

async function readSnapshot(page: import('@playwright/test').Page): Promise<string | null> {
	return page.evaluate(
		({ key, contextKey }) => {
			const context = window.sessionStorage.getItem(contextKey);
			return context ? window.sessionStorage.getItem(`${key}.${context}`) : null;
		},
		{ key: SNAPSHOT_KEY, contextKey: AUTH_CONTEXT_KEY }
	);
}

test('offline retry renders the authenticated tab snapshot with a stale badge', async ({ page }) => {
	let failNext = false;
	await page.route('**/api/library', (route) => {
		if (failNext) return route.abort('failed');
		failNext = true;
		return route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(SEEDED.items)
		});
	});

	await page.goto('/library');
	await expect(page.getByTestId('library-list')).toContainText('Seeded Song One');
	await page.getByRole('button', { name: 'Refresh' }).click();

	const list = page.getByTestId('library-list');
	await expect(list, 'stale library list missing').toBeVisible();
	await expect(page.getByTestId('library-item')).toHaveCount(2);
	const badge = page.getByTestId('library-stale-badge');
	await expect(badge, 'stale badge missing').toBeVisible();
	await expect(badge).toContainText(/stale — updated /i);
});

test('401 clears the scoped snapshot instead of showing another session library', async ({ page }) => {
	await seedSnapshot(page);
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 401,
			contentType: 'application/json',
			body: JSON.stringify({ error: 'unauthorized' })
		})
	);

	await page.goto('/library');

	await expect(page.getByRole('alert')).toContainText(/signed out|do not have access/i);
	await expect(page.getByTestId('library-list')).toHaveCount(0);
	await expect(page.getByTestId('library-stale-badge')).toHaveCount(0);
	expect(await readSnapshot(page)).toBeNull();
});

test('auth transition invalidates a previously authenticated tab snapshot', async ({ page }) => {
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(SEEDED.items)
		})
	);
	await page.goto('/library');
	await expect(page.getByTestId('library-list')).toContainText('Seeded Song One');

	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate((key) => {
		window.localStorage.setItem(key, 'test-auth-transition');
		window.localStorage.removeItem(key);
	}, 'nntune.library.auth-transition.v1');

	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('library-list')).toHaveCount(0);
	expect(await readSnapshot(page)).toBeNull();
	await otherTab.close();
});

test('auth transition discards an in-flight response body from the previous session', async ({
	page
}) => {
	await seedSnapshot(page);
	await page.addInitScript((items) => {
		const originalFetch = window.fetch.bind(window);
		window.fetch = ((input: RequestInfo | URL, init?: RequestInit) => {
			if (String(input).endsWith('/api/library')) {
				return Promise.resolve({
					ok: true,
					status: 200,
					json: () =>
						new Promise((resolve) => {
							Object.assign(window, { __releaseLibraryBody: () => resolve(items) });
						})
				} as Response);
			}
			return originalFetch(input, init);
		}) as typeof fetch;
	}, SEEDED.items);

	await page.goto('/library');
	await expect
		.poll(() => page.evaluate(() => typeof Reflect.get(window, '__releaseLibraryBody')))
		.toBe('function');

	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate((key) => {
		window.localStorage.setItem(key, 'test-auth-transition');
		window.localStorage.removeItem(key);
	}, 'nntune.library.auth-transition.v1');

	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await page.evaluate(() => Reflect.get(window, '__releaseLibraryBody')());
	await expect(page.getByTestId('library-list')).toHaveCount(0);
	expect(await readSnapshot(page)).toBeNull();
	await otherTab.close();
});

test('non-empty payload with no valid rows preserves the last-good snapshot', async ({ page }) => {
	await seedSnapshot(page);
	let invalid = false;
	await page.route('**/api/library', (route) => {
		if (invalid) {
			return route.fulfill({
				status: 200,
				contentType: 'application/json',
				body: JSON.stringify([{ track_id: 'foreign-row' }])
			});
		}
		invalid = true;
		return route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(SEEDED.items)
		});
	});

	await page.goto('/library');
	await page.getByRole('button', { name: 'Refresh' }).click();

	await expect(page.getByTestId('library-list')).toContainText('Seeded Song One');
	await expect(page.getByTestId('library-stale-badge')).toBeVisible();
	const stored = await readSnapshot(page);
	expect(JSON.parse(stored as string).items.map((i: { id: string }) => i.id)).toEqual([
		'track-seed-1',
		'track-seed-2'
	]);
});

test('offline load without session verification does not render a prior account snapshot', async ({
	page
}) => {
	await seedSnapshot(page);
	await page.route('**/api/library', (route) => route.abort('failed'));

	await page.goto('/library');

	await expect(page.getByRole('alert')).toContainText(/not reachable/i);
	await expect(page.getByTestId('library-list')).toHaveCount(0);
	await expect(page.getByTestId('library-stale-badge')).toHaveCount(0);
});

test('missing library endpoint stays neutral and does not expose a cached snapshot', async ({
	page
}) => {
	await seedSnapshot(page);
	await page.route('**/api/library', (route) =>
		route.fulfill({ status: 404, contentType: 'application/json', body: JSON.stringify({}) })
	);

	await page.goto('/library');

	await expect(page.getByRole('status')).toContainText(/listing is not available yet/i);
	await expect(page.getByRole('alert')).toHaveCount(0);
	await expect(page.getByTestId('library-list')).toHaveCount(0);
	await expect(page.getByTestId('library-stale-badge')).toHaveCount(0);
});

test('empty library keeps the NZB import link', async ({ page }) => {
	await page.route('**/api/library', (route) =>
		route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify([]) })
	);

	await page.goto('/library');

	await expect(page.getByRole('status')).toContainText(/library is empty/i);
	await expect(page.getByRole('link', { name: 'Import an NZB' })).toHaveAttribute(
		'href',
		'/library/import'
	);
});

test('a never-settling library request times out and exits loading state', async ({ page }) => {
	await page.route('**/api/library', async () => new Promise(() => {}));

	await page.goto('/library');

	await expect(page.getByRole('alert')).toContainText(/not reachable/i, { timeout: 12_000 });
	await expect(page.getByRole('status')).toHaveCount(0);
});

test('stale retry keeps every freshly loaded item beyond the persisted cache cap', async ({
	page
}) => {
	const fresh = Array.from({ length: 250 }, (_, index) => ({
		id: `track-${index}`,
		title: `Song ${index}`
	}));
	let failNext = false;
	await page.route('**/api/library', (route) => {
		if (failNext) return route.abort('failed');
		failNext = true;
		return route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(fresh)
		});
	});

	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(250);
	await page.getByRole('button', { name: 'Refresh' }).click();

	await expect(page.getByTestId('library-stale-badge')).toBeVisible();
	await expect(page.getByTestId('library-item')).toHaveCount(250);
	const stored = await readSnapshot(page);
	expect(JSON.parse(stored as string).items).toHaveLength(200);
});

test('fresh library success replaces the snapshot and hides the badge', async ({ page }) => {
	const fresh = [{ id: 'track-fresh-1', title: 'Fresh Song' }];
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(fresh)
		})
	);

	await page.goto('/library');

	await expect(page.getByTestId('library-list'), 'fresh library list missing').toBeVisible();
	await expect(page.getByTestId('library-item')).toHaveCount(1);
	await expect(page.getByTestId('library-list')).toContainText('Fresh Song');
	await expect(page.getByTestId('library-stale-badge'), 'badge should hide on fresh').toHaveCount(
		0
	);

	const stored = await readSnapshot(page);
	expect(stored, 'fresh fetch did not persist a snapshot').not.toBeNull();
	expect(JSON.parse(stored as string).items.map((i: { id: string }) => i.id)).toEqual([
		'track-fresh-1'
	]);
});

test('magic-link redemption invalidates another tab library snapshot', async ({ page }) => {
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(SEEDED.items)
		})
	);
	await page.goto('/library');
	await expect(page.getByTestId('library-list')).toContainText('Seeded Song One');

	const magicTab = await page.context().newPage();
	await magicTab.route('**/v1/auth/magic/finish', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({ ok: true, email: 'new-account@example.test' })
		})
	);
	// Keep this test focused on cross-tab cache invalidation, not the destination page.
	await magicTab.route('**/onboarding/provider', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'text/html',
			body: '<!doctype html><html><body></body></html>'
		})
	);
	await magicTab.goto('/auth/magic?token=test-magic-token');
	await magicTab.getByRole('button', { name: 'Sign in with this link' }).click();
	await expect(magicTab).toHaveURL(/\/onboarding\/provider$/);

	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('library-list')).toHaveCount(0);
	expect(await readSnapshot(page)).toBeNull();
	await magicTab.close();
});

test('magic-link sign-out invalidates another tab library snapshot', async ({ page }) => {
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(SEEDED.items)
		})
	);
	await page.goto('/library');
	await expect(page.getByTestId('library-list')).toContainText('Seeded Song One');

	const magicTab = await page.context().newPage();
	await magicTab.route('**/v1/auth/magic/finish', (route) =>
		route.fulfill({ status: 401, contentType: 'application/json', body: JSON.stringify({}) })
	);
	await magicTab.route('**/v1/session/logout', (route) =>
		route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({}) })
	);
	await magicTab.goto('/auth/magic?token=test-magic-token');
	await magicTab.getByRole('button', { name: 'Sign in with this link' }).click();
	await magicTab.getByRole('alert').waitFor();
	await magicTab.getByRole('button', { name: /sign out and try this link again/i }).click();

	await expect(magicTab.getByRole('status')).toContainText('Signed out');
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('library-list')).toHaveCount(0);
	expect(await readSnapshot(page)).toBeNull();
	await magicTab.close();
});

/** Make `window.sessionStorage` / `localStorage` throw on access, like a blocked origin. */
async function blockStorage(page: import('@playwright/test').Page): Promise<void> {
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
}

test('blocked storage still renders a fresh library list', async ({ page }) => {
	await blockStorage(page);
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(SEEDED.items)
		})
	);

	await page.goto('/library');

	await expect(page.getByTestId('library-list')).toContainText('Seeded Song One');
	await expect(page.getByTestId('library-item')).toHaveCount(2);
	await expect(page.getByRole('alert')).toHaveCount(0);
});

test('blocked storage retains a fresh list as stale after a failed refresh', async ({ page }) => {
	await blockStorage(page);
	let failNext = false;
	await page.route('**/api/library', (route) => {
		if (failNext) return route.abort('failed');
		failNext = true;
		return route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(SEEDED.items)
		});
	});

	await page.goto('/library');
	await expect(page.getByTestId('library-list')).toContainText('Seeded Song One');
	await page.getByRole('button', { name: 'Refresh' }).click();

	await expect(page.getByTestId('library-list')).toContainText('Seeded Song One');
	await expect(page.getByTestId('library-stale-badge')).toBeVisible();
	await expect(page.getByRole('alert')).toHaveCount(0);
});

test('blocked storage still shows the signed-out state on 401', async ({ page }) => {
	await blockStorage(page);
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 401,
			contentType: 'application/json',
			body: JSON.stringify({ error: 'unauthorized' })
		})
	);

	await page.goto('/library');

	await expect(page.getByRole('alert')).toContainText(/signed out|do not have access/i);
	await expect(page.getByTestId('library-list')).toHaveCount(0);
});

test('blocked storage does not turn a successful magic-link sign-in into a failure', async ({
	page
}) => {
	await blockStorage(page);
	await page.route('**/v1/auth/magic/finish', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({ ok: true, email: 'ada@example.com' })
		})
	);

	await page.goto('/auth/magic?token=test-magic-token');
	await page.getByRole('button', { name: /sign in with this link/i }).click();

	// Cleanup after a successful redemption must not land in the error branch.
	await page.waitForURL('/onboarding/byop');
});