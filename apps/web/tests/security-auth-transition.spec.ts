import { expect, test, type Page } from '@playwright/test';

/**
 * Auth-transition acceptance: a qualifying security result (logout or
 * revoke-all fulfilled success, or a typed HTTP401) purges this tab's
 * accessible auth state and resets every mounted consumer — library,
 * provider and BYOP — even when the security route was abandoned first.
 * The abandoned route itself stays silent: no navigation, error, message
 * or busy-state writes. Non-qualifying results notify nothing.
 *
 * Transport contract pinned here: one shared same-window dispatch plus the
 * existing localStorage transition for other tabs. Receivers never
 * rebroadcast, request, navigate or retry; delivery is best effort. Every
 * held request and event below is released explicitly; bounded waits cover
 * only negative observation, never race outcomes. All API destinations are
 * local fixtures, never live services.
 *
 * Breaks caught: an abandoned qualifying result that leaves snapshots,
 * identity labels or drafts behind; a late library/provider response that
 * repopulates a reset view; a stale BYOP test/save/reauth continuation
 * that runs after reset; a retained paywall retry that resubmits; a new
 * paywall published by a late response; a rebroadcast/request loop; a
 * storage denial that suppresses the local reset or throws.
 */
const SNAPSHOT = 'nntune.library.snapshot.v1';
const AUTH_CONTEXT = 'nntune.library.auth-context.v1';
const TRANSITION = 'nntune.library.auth-transition.v1';
const EMAIL = 'nntune:signed-in-email';
const RESTART_NOTICE =
	'This setup was reset after an authentication change. Sign in and start setup again; ' +
	're-enter and retest your credentials. Requests already submitted may still complete.';

interface HeldRequest {
	method: string;
	path: string;
	body: unknown;
}

interface TransitionProbe {
	requests: HeldRequest[];
	rejections: string[];
	writes: string[];
	broadcasts: string[];
	release: (path: string, status: number, body: unknown) => void;
}

declare global {
	interface Window {
		__transition: TransitionProbe;
	}
}

type HoldRule = string | { path: string; methods?: string[]; passFirst?: number };

/**
 * Hold the fetch boundary for `holds`; the real clients, handlers,
 * Svelte teardown, routing and cache code all run. The held transport
 * deliberately ignores cancellation: a late response still needs a page
 * or client guard. Anything under /v1/** that is not held fails loudly so
 * a stray request cannot hide behind a benign fixture.
 *
 * A rule can narrow by HTTP method and can pass the first N matches
 * through (the security page load probes GET /v1/vault before any held
 * action runs, so a vault hold must not swallow that probe).
 */
async function fixtures(page: Page, holds: HoldRule[]) {
	const normalized = holds.map((rule) =>
		typeof rule === 'string'
			? { path: rule, methods: null as string[] | null, pass: 0 }
			: { path: rule.path, methods: rule.methods ?? null, pass: rule.passFirst ?? 0 }
	);
	const pageErrors: string[] = [];
	const navigations: string[] = [];
	page.on('pageerror', (error) => pageErrors.push(error.message));
	page.on('framenavigated', (frame) => {
		if (frame === page.mainFrame()) navigations.push(frame.url());
	});
	await page.route('**/v1/**', async (route) => {
		await route.fulfill({
			status: 500,
			json: { code: 'unexpected_fixture_request', message: new URL(route.request().url()).pathname }
		});
	});
	await page.addInitScript(({ rules }) => {
		const nativeFetch = window.fetch.bind(window);
		const pending: Array<{ path: string; settle: (response: Response) => void }> = [];
		const probe: TransitionProbe = {
			requests: [],
			rejections: [],
			writes: [],
			broadcasts: [],
			release: () => {
				throw new Error('No held request');
			}
		};
		window.__transition = probe;
		window.addEventListener('unhandledrejection', (event) =>
			probe.rejections.push(String(event.reason))
		);
		window.fetch = (input, init) => {
			const request = new Request(input, init);
			const pathname = new URL(request.url).pathname;
			const rule = (rules as Array<{ path: string; methods: string[] | null; pass: number }>).find(
				(candidate) =>
					candidate.path === pathname &&
					(candidate.methods === null || candidate.methods.includes(request.method))
			);
			if (!rule) return nativeFetch(input, init);
			if (rule.pass > 0) {
				rule.pass -= 1;
				return nativeFetch(input, init);
			}
			return new Promise<Response>((resolve) => {
				const park = (body: unknown) => {
					probe.requests.push({ method: request.method, path: pathname, body });
					pending.push({
						path: pathname,
						settle: (response) => resolve(response)
					});
				};
				if (request.method === 'GET' || request.method === 'HEAD') park(null);
				else void request.clone().json().then(park, () => park(null));
			});
		};
		probe.release = (path, status, responseBody) => {
			const index = pending.findIndex((item) => item.path === path);
			if (index < 0) throw new Error(`No held request for ${path}`);
			const [item] = pending.splice(index, 1);
			item.settle(
				new Response(JSON.stringify(responseBody), {
					status,
					headers: { 'content-type': 'application/json' }
				})
			);
		};
		for (const method of ['setItem', 'removeItem', 'clear'] as const) {
			const original = Storage.prototype[method];
			// Preserve the real storage side effects; observe writes, not mocks.
			Object.defineProperty(Storage.prototype, method, {
				configurable: true,
				value: function (this: Storage, ...args: string[]) {
					probe.writes.push(`${method}:${args[0] ?? ''}`);
					return Reflect.apply(original, this, args);
				}
			});
		}
		const broadcast = BroadcastChannel.prototype.postMessage;
		BroadcastChannel.prototype.postMessage = function (message) {
			probe.broadcasts.push(this.name);
			return broadcast.call(this, message);
		};
	}, { rules: normalized });
	return { pageErrors, navigations };
}

async function seedState(page: Page) {
	await page.evaluate(
		({ snapshot, context, email }) => {
			for (const storage of [sessionStorage, localStorage]) {
				storage.setItem(context, 'transition-account');
				storage.setItem(
					`${snapshot}.transition-account`,
					JSON.stringify({
						savedAt: '2026-10-19T12:00:00Z',
						items: [{ id: 'held-track', title: 'Keep this snapshot' }]
					})
				);
				storage.setItem(snapshot, 'legacy snapshot fixture');
			}
			sessionStorage.setItem(email, 'fixture@example.com');
		},
		{ snapshot: SNAPSHOT, context: AUTH_CONTEXT, email: EMAIL }
	);
}

async function storedState(page: Page) {
	return page.evaluate(
		({ snapshot, context, email }) => ({
			areas: [sessionStorage, localStorage].map((storage) => ({
				context: storage.getItem(context),
				scoped: storage.getItem(`${snapshot}.transition-account`),
				legacy: storage.getItem(snapshot)
			})),
			email: sessionStorage.getItem(email)
		}),
		{ snapshot: SNAPSHOT, context: AUTH_CONTEXT, email: EMAIL }
	);
}

async function readEmail(page: Page) {
	return page.evaluate((key) => sessionStorage.getItem(key), EMAIL);
}

async function requestCount(page: Page, path: string) {
	return page.evaluate(
		(wanted) => window.__transition.requests.filter((item) => item.path === wanted).length,
		path
	);
}

async function beginSecurity(page: Page, kind: 'logout' | 'revoke-all') {
	const endpoint = kind === 'logout' ? '/v1/session/logout' : '/v1/session/revoke-all';
	const button =
		kind === 'logout' ? /^Sign out$/i : /^Sign out everywhere$/i;
	await page.goto('/account/security');
	const control = page.getByRole('button', { name: button });
	await expect(control).toBeEnabled();
	await seedState(page);
	// Accessible names change to Signing out…/Signing out everywhere… while
	// busy, so pin the element before clicking instead of re-resolving it.
	const element = await control.elementHandle();
	if (!element) throw new Error(`Missing security button: ${kind}`);
	if (kind === 'revoke-all') page.once('dialog', (dialog) => dialog.accept());
	await control.click();
	await expect
		.poll(() => page.evaluate(() => window.__transition.requests))
		.toEqual([{ method: 'POST', path: endpoint, body: {} }]);
	await expect.poll(() => element.evaluate((node) => (node as HTMLButtonElement).disabled)).toBe(true);
	await element.dispose();
	return endpoint;
}

/** A real delegated anchor click, not page.goto/history-only URL replacement. */
async function clientNavigate(page: Page, path: string) {
	await page.evaluate((href) => {
		const link = document.createElement('a');
		link.href = href;
		link.textContent = 'Leave held action';
		link.dataset.testid = 'leave-held-action';
		document.body.append(link);
	}, path);
	await page.getByTestId('leave-held-action').click();
	await expect(page).toHaveURL((url) => url.pathname + url.search === path);
	await page.getByTestId('leave-held-action').evaluate((link) => link.remove());
	// This exact probe lives only in the original JS realm. A full reload
	// would lose the pending request and make the negative test meaningless.
	expect(await page.evaluate(() => window.__transition.requests.length)).toBeGreaterThan(0);
}

async function release(page: Page, path: string, status: number, body: unknown) {
	await page.evaluate(
		({ path, status, body }) => {
			window.__transition.release(path, status, body);
		},
		{ path, status, body }
	);
}

/** Fill the BYOP wizard to step 3 with distinct probe secrets. */
async function fillByopDraft(page: Page, tag: string) {
	await page.getByLabel('Hostname').fill(`news.${tag}.example.test`);
	await page.getByLabel('Username').fill(`${tag}-reader`);
	await page.getByLabel('Password', { exact: true }).fill(`${tag}-probe-password`);
	await page.getByRole('button', { name: 'Continue' }).click();
	await page.getByLabel('Indexer URL').fill(`https://indexer.${tag}.example.test`);
	await page.getByLabel('Indexer API key').fill(`${tag}-probe-indexer-key`);
	await page.getByRole('button', { name: 'Continue' }).click();
	await expect(page.getByRole('region', { name: 'Test the connection' })).toBeVisible();
}

/** A second tab in the same context performs a real live logout. */
async function actorLogout(page: Page) {
	const actor = await page.context().newPage();
	await actor.route('**/v1/vault', (route) =>
		route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({}) })
	);
	await actor.route('**/v1/session/logout', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({ ok: true })
		})
	);
	await actor.goto('/account/security');
	await actor.getByRole('button', { name: /^sign out$/i }).click();
	await actor.waitForURL('**/login');
	await actor.close();
}

async function breakStorage(page: Page, methods: string[]) {
	await page.evaluate((names) => {
		for (const name of names as string[]) {
			Object.defineProperty(Storage.prototype, name, {
				configurable: true,
				value: function () {
					throw new DOMException('fixture storage denied', 'SecurityError');
				}
			});
		}
	}, methods);
}

async function probeSnapshot(page: Page) {
	return page.evaluate(() => ({
		writes: window.__transition.writes,
		broadcasts: window.__transition.broadcasts,
		rejections: window.__transition.rejections,
		requests: window.__transition.requests
	}));
}

test('abandoned logout success resets the same-window replacement library and fences its late response', async ({
	page
}) => {
	const observed = await fixtures(page, ['/v1/session/logout', '/api/library']);
	const endpoint = await beginSecurity(page, 'logout');
	await clientNavigate(page, '/library');
	await expect(page.getByText('Loading your library…')).toBeVisible();
	await expect.poll(() => requestCount(page, '/api/library')).toBe(1);
	observed.navigations.length = 0;
	await page.evaluate(() => {
		window.__transition.writes.length = 0;
		window.__transition.broadcasts.length = 0;
	});

	await release(page, endpoint, 200, { ok: true });

	// The purge landed and the mounted replacement reset to sign-in-required.
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByRole('link', { name: 'Sign in' })).toHaveAttribute('href', '/login');
	await expect(page.getByTestId('library-list')).toHaveCount(0);
	expect(await storedState(page)).toEqual({
		areas: [
			{ context: null, scoped: null, legacy: null },
			{ context: null, scoped: null, legacy: null }
		],
		email: null
	});
	expect.soft(observed.navigations, 'no event-driven navigation').toEqual([]);
	expect.soft(new URL(page.url()).pathname).toBe('/library');
	expect(await requestCount(page, endpoint)).toBe(1);

	// The library response that was already in flight cannot repopulate the
	// reset view, retry the fetch, or rebroadcast the transition.
	await release(page, '/api/library', 200, [{ id: 'late-track', title: 'Late arrival' }]);
	await page.waitForTimeout(350);
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('library-list')).toHaveCount(0);
	expect(await requestCount(page, '/api/library')).toBe(1);
	const probe = await probeSnapshot(page);
	expect(probe.writes.filter((entry) => entry === `setItem:${TRANSITION}`)).toHaveLength(1);
	expect.soft(probe.broadcasts, 'no BroadcastChannel relay').toEqual([]);
	expect.soft(probe.rejections).toEqual([]);
	expect.soft(observed.pageErrors).toEqual([]);
	expect.soft(observed.navigations, 'no late navigation').toEqual([]);
});

test('abandoned revoke-all success resets the same-window replacement provider and fences its late summary', async ({
	page
}) => {
	const observed = await fixtures(page, [
		'/v1/session/revoke-all',
		// The security load probes GET /v1/vault first; hold only the later
		// provider summary read.
		{ path: '/v1/vault', methods: ['GET'], passFirst: 1 }
	]);
	const endpoint = await beginSecurity(page, 'revoke-all');
	await clientNavigate(page, '/account/provider');
	await expect(page.getByText('Loading saved-provider status…')).toBeVisible();
	await expect.poll(() => requestCount(page, '/v1/vault')).toBe(1);
	observed.navigations.length = 0;
	await page.evaluate(() => {
		window.__transition.writes.length = 0;
		window.__transition.broadcasts.length = 0;
	});

	await release(page, endpoint, 200, { ok: true });

	// The existing expired presentation is preserved; the summary is gone.
	await expect(page.getByText('Your session has expired or changed.')).toBeVisible();
	await expect(page.getByRole('link', { name: 'Sign in again' })).toHaveAttribute(
		'href',
		'/login'
	);
	await expect(page.getByText('Saved provider configured')).toHaveCount(0);
	expect(await storedState(page)).toEqual({
		areas: [
			{ context: null, scoped: null, legacy: null },
			{ context: null, scoped: null, legacy: null }
		],
		email: null
	});
	expect.soft(observed.navigations, 'no event-driven navigation').toEqual([]);
	expect.soft(new URL(page.url()).pathname).toBe('/account/provider');
	expect(await requestCount(page, endpoint)).toBe(1);

	// The held summary response cannot repaint the expired view or refetch.
	await release(page, '/v1/vault', 200, {
		provider_configured: true,
		indexer_configured: false,
		updated_at: 1
	});
	await page.waitForTimeout(350);
	await expect(page.getByText('Your session has expired or changed.')).toBeVisible();
	await expect(page.getByText('Saved provider configured')).toHaveCount(0);
	expect(await requestCount(page, '/v1/vault')).toBe(1);
	const probe = await probeSnapshot(page);
	expect(probe.writes.filter((entry) => entry === `setItem:${TRANSITION}`)).toHaveLength(1);
	expect.soft(probe.broadcasts, 'no BroadcastChannel relay').toEqual([]);
	expect.soft(probe.rejections).toEqual([]);
	expect.soft(observed.pageErrors).toEqual([]);
	expect.soft(observed.navigations, 'no late navigation').toEqual([]);
});

test('abandoned revoke-all 401 purges and resets without any message or redirect', async ({
	page
}) => {
	const observed = await fixtures(page, ['/v1/session/revoke-all', '/api/library']);
	const endpoint = await beginSecurity(page, 'revoke-all');
	await clientNavigate(page, '/library');
	await expect(page.getByText('Loading your library…')).toBeVisible();
	await expect.poll(() => requestCount(page, '/api/library')).toBe(1);
	observed.navigations.length = 0;

	// A typed 401 is a qualifying result: conservative purge plus reset, but
	// the abandoned route renders no expired-session message and redirects
	// nowhere. It is not proof of global revocation either.
	await release(page, endpoint, 401, { code: 'unauthorized', message: 'held fixture failure' });
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('library-list')).toHaveCount(0);
	expect(await storedState(page)).toEqual({
		areas: [
			{ context: null, scoped: null, legacy: null },
			{ context: null, scoped: null, legacy: null }
		],
		email: null
	});
	await expect(page.getByText(/session expired before other devices/i)).toHaveCount(0);
	expect.soft(observed.navigations, 'no event-driven navigation').toEqual([]);
	expect.soft(new URL(page.url()).pathname).toBe('/library');
	expect(await requestCount(page, endpoint)).toBe(1);

	await release(page, '/api/library', 200, [{ id: 'late-track', title: 'Late arrival' }]);
	await page.waitForTimeout(350);
	await expect(page.getByTestId('library-list')).toHaveCount(0);
	expect(await requestCount(page, '/api/library')).toBe(1);
	const probe = await probeSnapshot(page);
	expect.soft(probe.rejections).toEqual([]);
	expect.soft(observed.pageErrors).toEqual([]);
});

test('abandoned security failure leaves the mounted replacement library untouched', async ({
	page
}) => {
	const observed = await fixtures(page, ['/v1/session/logout']);
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify([{ id: 'fresh-track', title: 'Fresh Song' }])
		})
	);
	const endpoint = await beginSecurity(page, 'logout');
	await clientNavigate(page, '/library');
	await expect(page.getByTestId('library-list')).toContainText('Fresh Song');
	const before = await storedState(page);
	observed.navigations.length = 0;

	// A non-qualifying failure notifies nothing: the mounted replacement keeps
	// its list, storage is preserved, and no error UI appears.
	await release(page, endpoint, 500, { code: 'internal_error', message: 'held fixture failure' });
	await page.waitForTimeout(350);
	await expect(page.getByTestId('library-list')).toContainText('Fresh Song');
	await expect(page.getByRole('alert')).toHaveCount(0);
	expect(await storedState(page)).toEqual(before);
	expect.soft(observed.navigations).toEqual([]);
	expect.soft(new URL(page.url()).pathname).toBe('/library');
	expect(await requestCount(page, endpoint)).toBe(1);
	const probe = await probeSnapshot(page);
	expect.soft(probe.rejections).toEqual([]);
	expect.soft(observed.pageErrors).toEqual([]);
});

test('abandoned logout in one tab invalidates an open library tab without navigating either', async ({
	page
}) => {
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify([{ id: 'seed-track', title: 'Seeded Song' }])
		})
	);
	await page.addInitScript((key) => {
		sessionStorage.setItem(key, 'library-tab@example.com');
	}, EMAIL);
	const libraryErrors: string[] = [];
	page.on('pageerror', (error) => libraryErrors.push(error.message));
	await page.goto('/library');
	await expect(page.getByTestId('library-list')).toContainText('Seeded Song');

	const securityTab = await page.context().newPage();
	const observed = await fixtures(securityTab, ['/v1/session/logout']);
	const endpoint = await beginSecurity(securityTab, 'logout');
	await clientNavigate(securityTab, '/');
	observed.navigations.length = 0;

	await release(securityTab, endpoint, 200, { ok: true });

	// The delivered transition clears the library tab's own snapshot, memory
	// and per-tab label; the acting tab purged but never navigated.
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('library-list')).toHaveCount(0);
	expect(await readEmail(page)).toBeNull();
	expect.soft(new URL(page.url()).pathname, 'receiver never navigates').toBe('/library');
	expect.soft(new URL(securityTab.url()).pathname, 'abandoned actor never navigates').toBe('/');
	await expect(securityTab.getByRole('alert')).toHaveCount(0);
	expect.soft(observed.navigations).toEqual([]);
	expect(await requestCount(securityTab, endpoint)).toBe(1);
	expect(await storedState(securityTab)).toEqual({
		areas: [
			{ context: null, scoped: null, legacy: null },
			{ context: null, scoped: null, legacy: null }
		],
		email: null
	});
	const probe = await probeSnapshot(securityTab);
	expect.soft(probe.rejections).toEqual([]);
	expect.soft(observed.pageErrors).toEqual([]);
	expect.soft(libraryErrors).toEqual([]);
	await securityTab.close();
});

test('a sign-in that lands before the delayed transition is intentionally expired', async ({
	page
}) => {
	let libraryPosts = 0;
	await page.route('**/api/library', async (route) => {
		libraryPosts += 1;
		await route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify([{ id: 'new-track', title: 'New Session Song' }])
		});
	});
	await page.addInitScript((key) => {
		sessionStorage.setItem(key, 'new-session@example.com');
	}, EMAIL);
	await page.goto('/library');
	await expect(page.getByTestId('library-list')).toContainText('New Session Song');
	expect(await readEmail(page)).toBe('new-session@example.com');

	// The sign-in above committed newer client-visible state; the delayed
	// transition deliberately expires it rather than preserving newer UI.
	await actorLogout(page);

	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('library-list')).toHaveCount(0);
	expect(await readEmail(page)).toBeNull();
	expect(libraryPosts).toBe(1);
	expect.soft(new URL(page.url()).pathname).toBe('/library');
});

test('after the transition, explicit revalidation resumes fresh work while old work stays fenced', async ({
	page
}) => {
	const observed = await fixtures(page, ['/v1/session/logout', '/api/library']);
	const endpoint = await beginSecurity(page, 'logout');
	await clientNavigate(page, '/library');
	await expect(page.getByText('Loading your library…')).toBeVisible();
	await expect.poll(() => requestCount(page, '/api/library')).toBe(1);
	await release(page, endpoint, 200, { ok: true });
	await expect(page.getByRole('alert')).toContainText(/session changed/i);

	// The old held response cannot touch the reset view...
	await release(page, '/api/library', 200, [{ id: 'old-track', title: 'Old arrival' }]);
	await page.waitForTimeout(350);
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('library-list')).toHaveCount(0);

	// ...but an explicit Retry starts ordinary fresh work on the same mount.
	await page.getByRole('button', { name: 'Retry' }).click();
	await expect.poll(() => requestCount(page, '/api/library')).toBe(2);
	await release(page, '/api/library', 200, [{ id: 'fresh-track', title: 'Fresh Song' }]);
	await expect(page.getByTestId('library-list')).toContainText('Fresh Song');
	await expect(page.getByRole('alert')).toHaveCount(0);
	expect(await requestCount(page, endpoint)).toBe(1);
	const probe = await probeSnapshot(page);
	expect.soft(probe.rejections).toEqual([]);
	expect.soft(observed.pageErrors).toEqual([]);
});

test('duplicate transitions settle with no requests, rebroadcasts or navigation', async ({
	page
}) => {
	let libraryPosts = 0;
	await page.route('**/api/library', async (route) => {
		libraryPosts += 1;
		await route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify([{ id: 'seed-track', title: 'Seeded Song' }])
		});
	});
	await page.addInitScript(
		({ email, transition }) => {
			sessionStorage.setItem(email, 'duplicate-tab@example.com');
			const seen: string[] = [];
			(window as unknown as { __seenWrites: string[] }).__seenWrites = seen;
			for (const method of ['setItem', 'removeItem'] as const) {
				const original = Storage.prototype[method];
				Object.defineProperty(Storage.prototype, method, {
					configurable: true,
					value: function (this: Storage, ...args: string[]) {
						seen.push(`${method}:${args[0] ?? ''}`);
						return Reflect.apply(original, this, args);
					}
				});
			}
			void transition;
		},
		{ email: EMAIL, transition: TRANSITION }
	);
	await page.goto('/library');
	await expect(page.getByTestId('library-list')).toContainText('Seeded Song');

	await actorLogout(page);
	await actorLogout(page);

	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('library-list')).toHaveCount(0);
	expect(await readEmail(page)).toBeNull();
	expect(libraryPosts).toBe(1);
	const seen = await page.evaluate(
		() => (window as unknown as { __seenWrites: string[] }).__seenWrites
	);
	expect(
		seen.filter((entry) => entry === `setItem:${TRANSITION}`),
		'receivers never rebroadcast'
	).toEqual([]);
	expect.soft(new URL(page.url()).pathname).toBe('/library');
});

for (const denied of [['setItem', 'removeItem'], ['getItem']] as const) {
	test(`denied storage (${denied.join('/')}) still resets local consumers without throwing`, async ({
		page
	}) => {
		const observed = await fixtures(page, ['/v1/session/logout', '/api/library']);
		const endpoint = await beginSecurity(page, 'logout');
		// Seed first: the denial below must not break the fixture setup, only
		// the purge/broadcast the release triggers.
		await breakStorage(page, [...denied]);
		await clientNavigate(page, '/library');
		await expect.poll(() => requestCount(page, '/api/library')).toBe(1);

		await release(page, endpoint, 200, { ok: true });

		// The local reset is independent of storage success: the mounted
		// replacement expires in memory even though the purge cannot write.
		await expect(page.getByRole('alert')).toContainText(/session changed/i);
		await expect(page.getByTestId('library-list')).toHaveCount(0);
		expect.soft(observed.navigations, 'no event-driven navigation').toEqual([]);
		expect(await requestCount(page, endpoint)).toBe(1);
		const probe = await probeSnapshot(page);
		expect.soft(probe.rejections, 'no unhandled rejection').toEqual([]);
		expect.soft(observed.pageErrors, 'no page error').toEqual([]);
	});
}

test('abandoned logout resets the same-window BYOP draft and fences its pending test', async ({
	page
}) => {
	const observed = await fixtures(page, ['/v1/session/logout', '/v1/vault/test']);
	let saves = 0;
	let reauthPosts = 0;
	await page.route('**/v1/vault', async (route) => {
		saves += 1;
		await route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({ ok: true })
		});
	});
	await page.route('**/v1/session/reauthenticate/**', async (route) => {
		reauthPosts += 1;
		await route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({ ok: true })
		});
	});
	const endpoint = await beginSecurity(page, 'logout');
	await clientNavigate(page, '/onboarding/byop');
	await expect(page.getByText('Signed in as')).toBeVisible();
	await fillByopDraft(page, 'fence');
	await page.getByTestId('byop-test-connection').click();
	await expect.poll(() => requestCount(page, '/v1/vault/test')).toBe(1);
	observed.navigations.length = 0;

	await release(page, endpoint, 200, { ok: true });

	// Fail-closed restart: neutral notice with an explicit sign-in link, no
	// redirect; identity, secrets and tested state cleared to step-1 defaults.
	const notice = page.getByText(RESTART_NOTICE);
	await expect(notice).toBeVisible();
	await expect(page.locator('a[href="/login"]')).toHaveCount(1);
	await expect(page.getByText('Signed in as')).toHaveCount(0);
	await expect(page.getByText('Step 1 of 3')).toBeVisible();
	await expect(page.getByLabel('Hostname')).toHaveValue('');
	await expect(page.getByLabel('Username')).toHaveValue('');
	await expect(page.getByLabel('Password', { exact: true })).toHaveValue('');
	expect(await readEmail(page)).toBeNull();
	expect(await requestCount(page, '/v1/vault/test')).toBe(1);
	expect(saves).toBe(0);
	expect(reauthPosts).toBe(0);
	expect.soft(observed.navigations, 'no event-driven navigation').toEqual([]);
	expect.soft(new URL(page.url()).pathname).toBe('/onboarding/byop');

	// The held test response cannot restore tested state, identity, status
	// or secrets, publish a paywall, or chain another request.
	await release(page, '/v1/vault/test', 200, { ok: true, latencyMs: 5 });
	await page.waitForTimeout(350);
	await expect(notice).toBeVisible();
	await expect(page.getByTestId('byop-save')).toHaveCount(0);
	await expect(page.getByText(/checks passed/i)).toHaveCount(0);
	await expect(page.getByRole('dialog')).toHaveCount(0);
	expect(await requestCount(page, '/v1/vault/test')).toBe(1);
	expect(saves).toBe(0);
	expect(reauthPosts).toBe(0);

	// Programmatic handler paths cannot bypass the guard either: rendered
	// controls stay disabled and direct activation dispatches nothing.
	const testControl = page.getByTestId('byop-test-connection');
	if ((await testControl.count()) > 0) await expect(testControl).toBeDisabled();
	await page.evaluate(() => {
		document.querySelector('[data-testid="byop-test-connection"]')?.dispatchEvent(
			new MouseEvent('click', { bubbles: true })
		);
		document.querySelector('[data-testid="byop-save"]')?.dispatchEvent(
			new MouseEvent('click', { bubbles: true })
		);
	});
	await page.getByLabel('Password', { exact: true }).press('Enter');
	await page.waitForTimeout(350);
	expect(await requestCount(page, '/v1/vault/test')).toBe(1);
	expect(saves).toBe(0);
	expect(reauthPosts).toBe(0);
	await expect(notice).toBeVisible();
	const probe = await probeSnapshot(page);
	expect.soft(probe.rejections).toEqual([]);
	expect.soft(observed.pageErrors).toEqual([]);
	expect.soft(observed.navigations, 'no late navigation').toEqual([]);
});

test('abandoned logout clears tested state and fences an already-dispatched save', async ({
	page
}) => {
	const observed = await fixtures(page, [
		'/v1/session/logout',
		// Only the save PUT is held; the security load probes GET /v1/vault
		// and must fail fast through the catch-all instead of hanging.
		{ path: '/v1/vault', methods: ['PUT'] }
	]);
	await page.route('**/v1/vault/test', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({ ok: true, latencyMs: 5 })
		})
	);
	const endpoint = await beginSecurity(page, 'logout');
	await clientNavigate(page, '/onboarding/byop');
	await fillByopDraft(page, 'save');
	await page.getByTestId('byop-test-connection').click();
	await expect(page.getByTestId('byop-save')).toBeEnabled();
	await page.getByTestId('byop-save').click();
	await expect.poll(() => requestCount(page, '/v1/vault')).toBe(1);
	observed.navigations.length = 0;

	await release(page, endpoint, 200, { ok: true });

	const notice = page.getByText(RESTART_NOTICE);
	await expect(notice).toBeVisible();
	await expect(page.locator('a[href="/login"]')).toHaveCount(1);
	await expect(page.getByTestId('byop-save')).toHaveCount(0);
	expect(await readEmail(page)).toBeNull();

	// The save was already dispatched exactly once and may still complete
	// server-side; locally it must settle to nothing: no success UI, no
	// navigation, no replay, no chained request.
	await release(page, '/v1/vault', 200, { ok: true });
	await page.waitForTimeout(350);
	expect.soft(new URL(page.url()).pathname).toBe('/onboarding/byop');
	expect.soft(observed.navigations, 'no event-driven navigation').toEqual([]);
	await expect(notice).toBeVisible();
	await expect(page.getByText(/credentials are sealed/i)).toHaveCount(0);
	await expect(page.getByTestId('byop-save')).toHaveCount(0);
	expect(await requestCount(page, '/v1/vault')).toBe(1);
	expect(await requestCount(page, endpoint)).toBe(1);

	// A queued submit retaining the old draft dispatches nothing after reset.
	await page.evaluate(() => {
		document.querySelector('[data-testid="byop-save"]')?.dispatchEvent(
			new MouseEvent('click', { bubbles: true })
		);
		document.querySelector('[data-testid="byop-test-connection"]')?.dispatchEvent(
			new MouseEvent('click', { bubbles: true })
		);
	});
	await page.waitForTimeout(350);
	expect(await requestCount(page, '/v1/vault')).toBe(1);
	const probe = await probeSnapshot(page);
	expect.soft(probe.rejections).toEqual([]);
	expect.soft(observed.pageErrors).toEqual([]);
});

test('abandoned security failure leaves the BYOP draft and tested state alone', async ({
	page
}) => {
	const observed = await fixtures(page, ['/v1/session/logout']);
	await page.route('**/v1/vault/test', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({ ok: true, latencyMs: 5 })
		})
	);
	const endpoint = await beginSecurity(page, 'logout');
	await clientNavigate(page, '/onboarding/byop');
	await fillByopDraft(page, 'intact');
	await page.getByTestId('byop-test-connection').click();
	await expect(page.getByTestId('byop-save')).toBeEnabled();
	const before = await storedState(page);

	await release(page, endpoint, 500, { code: 'internal_error', message: 'held fixture failure' });
	await page.waitForTimeout(350);

	// Non-qualifying: no reset, no notice, tested state and typed draft
	// survive, storage is preserved.
	await expect(page.getByTestId('byop-save')).toBeEnabled();
	await expect(page.getByText(/reset after an authentication change/i)).toHaveCount(0);
	await page.getByRole('button', { name: 'Back' }).click();
	await page.getByRole('button', { name: 'Back' }).click();
	await expect(page.getByLabel('Password', { exact: true })).toHaveValue('intact-probe-password');
	expect(await storedState(page)).toEqual(before);
	expect(await requestCount(page, endpoint)).toBe(1);
	const probe = await probeSnapshot(page);
	expect.soft(probe.rejections).toEqual([]);
	expect.soft(observed.pageErrors).toEqual([]);
});
