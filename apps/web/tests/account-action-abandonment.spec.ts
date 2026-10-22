import { expect, test, type Page } from '@playwright/test';

/**
 * Route-lifetime acceptance, intentionally independent of the page handlers.
 * Hold only the HTTP boundary; the real billing/auth clients, event handlers,
 * Svelte teardown, routing and cache invalidation all run. The held transport
 * deliberately ignores cancellation: a late response still needs a page guard.
 * All API and Stripe destinations are local fixtures, never live services.
 *
 * Breaks caught: a removed success/401/error liveness guard, stale auth-cache
 * invalidation, a stale error appearing on a replacement instance, or a guard
 * which also suppresses the live success/error controls. Detached private
 * rune assignments are not directly observable; state assertions below cover
 * storage and the replacement page's visible state, not source-text shapes.
 *
 * Security-result classification: a qualifying security result (logout or
 * revoke-all fulfilled success, or a typed HTTP401) crosses the auth boundary
 * even when the security route has been abandoned — accessible snapshots,
 * context and the per-tab identity label are purged and existing consumers
 * are notified — while the abandoned route itself stays silent (no
 * navigation, error, message or busy-state writes). Non-qualifying results
 * (other HTTP failures, network rejection, malformed bodies) purge nothing.
 * Pricing and billing abandonment keep the full preservation contract.
 */
const SNAPSHOT = 'nntune.library.snapshot.v1';
const AUTH_CONTEXT = 'nntune.library.auth-context.v1';
const EMAIL = 'nntune:signed-in-email';
const CHECKOUT_URL = 'https://checkout.stripe.com/c/pay/abandonment-fixture';
const PORTAL_URL = 'https://billing.stripe.com/p/session/abandonment-fixture';
const PLANS = [
	{ id: 'free', priceCents: 0, streamHoursMonth: 1, maxConcurrentStreams: 1, maxLibraryTracks: 10, ingestJobsDay: 1, cachePriority: 'normal' },
	{ id: 'plus', priceCents: 500, streamHoursMonth: 100, maxConcurrentStreams: 2, maxLibraryTracks: 1000, ingestJobsDay: 20, cachePriority: 'normal' },
	{ id: 'pro-5x', priceCents: 2500, streamHoursMonth: 500, maxConcurrentStreams: 5, maxLibraryTracks: 5000, ingestJobsDay: 100, cachePriority: 'high' },
	{ id: 'pro-20x', priceCents: 10000, streamHoursMonth: 2000, maxConcurrentStreams: 20, maxLibraryTracks: 20000, ingestJobsDay: 400, cachePriority: 'high' }
];

interface Action {
	name: string;
	path: string;
	endpoint: string;
	button: RegExp;
	body: Record<string, string>;
	destination: string;
	confirm?: boolean;
}
const ACTIONS: Action[] = [
	{ name: 'pricing checkout', path: '/pricing', endpoint: '/v1/billing/checkout', button: /^Choose Plus$/, body: { planId: 'plus' }, destination: CHECKOUT_URL },
	{ name: 'billing checkout', path: '/account/billing', endpoint: '/v1/billing/checkout', button: /^Upgrade to plus$/i, body: { planId: 'plus' }, destination: CHECKOUT_URL },
	{ name: 'billing portal', path: '/account/billing', endpoint: '/v1/billing/portal', button: /^Manage billing$/, body: {}, destination: PORTAL_URL },
	{ name: 'logout', path: '/account/security', endpoint: '/v1/session/logout', button: /^Sign out$/i, body: {}, destination: '/login' },
	{ name: 'revoke all', path: '/account/security', endpoint: '/v1/session/revoke-all', button: /^Sign out everywhere$/i, body: {}, destination: '/login', confirm: true }
];

interface Probe {
	requests: Array<{ method: string; path: string; body: unknown }>;
	rejections: string[];
	writes: string[];
	broadcasts: string[];
	release: (status: number, body: unknown) => void;
	releaseRaw: (status: number, rawBody: string) => void;
	releaseReject: () => void;
}
declare global {
	interface Window { __abandonment: Probe }
}

async function fixtures(page: Page, action: Action, allowExternal = false) {
	const pageErrors: string[] = [];
	const navigations: string[] = [];
	page.on('pageerror', (error) => pageErrors.push(error.message));
	page.on('framenavigated', (frame) => {
		if (frame === page.mainFrame()) navigations.push(frame.url());
	});
	const external: string[] = [];
	for (const host of ['checkout.stripe.com', 'billing.stripe.com']) {
		await page.route(`https://${host}/**`, async (route) => {
			external.push(route.request().url());
			if (allowExternal) {
				await route.fulfill({ contentType: 'text/html', body: '<h1>Local Stripe destination</h1>' });
			} else {
				// Record even a navigation *attempt*, without unloading our probe.
				await route.abort('aborted');
			}
		});
	}
	await page.route('**/v1/**', async (route) => {
		const path = new URL(route.request().url()).pathname;
		const body = path === '/v1/plans' ? { plans: PLANS }
			: path === '/v1/me/subscription' ? { planId: 'free', status: 'trialing', currentPeriodEnd: null, trialEndsAt: '2030-01-01T00:00:00.000Z' }
			: path === '/v1/me/usage' ? { period: '2026-10', streamSecondsMonth: 0, concurrentNow: 0, libraryTracks: 0, ingestToday: 0 }
			: path === '/v1/vault' ? {}
			: { code: 'unexpected_fixture_request', message: path };
		await route.fulfill({ status: path === action.endpoint ? 500 : 200, json: body });
	});
	await page.route('**/api/library', (route) => route.fulfill({ json: [] }));
	await page.addInitScript(({ endpoint }) => {
		const nativeFetch = window.fetch.bind(window);
		const probe: Probe = {
			requests: [], rejections: [], writes: [], broadcasts: [],
			release: () => { throw new Error('No held request'); },
			releaseRaw: () => { throw new Error('No held request'); },
			releaseReject: () => { throw new Error('No held request'); }
		};
		window.__abandonment = probe;
		window.addEventListener('unhandledrejection', (event) => probe.rejections.push(String(event.reason)));
		window.fetch = (input, init) => {
			const request = new Request(input, init);
			if (new URL(request.url).pathname !== endpoint) return nativeFetch(input, init);
			return new Promise<Response>((resolve, reject) => {
				void request.clone().json().then((body) => {
					probe.requests.push({ method: request.method, path: endpoint, body });
					probe.release = (status, responseBody) => resolve(new Response(JSON.stringify(responseBody), {
						status, headers: { 'content-type': 'application/json' }
					}));
					probe.releaseRaw = (status, rawBody) => resolve(new Response(rawBody, {
						status, headers: { 'content-type': 'application/json' }
					}));
					probe.releaseReject = () => reject(new TypeError('fixture network failure'));
				});
			});
		};
		for (const method of ['setItem', 'removeItem', 'clear'] as const) {
			const original = Storage.prototype[method];
			// Preserve the real storage side effects; observe writes, not mocks.
			Object.defineProperty(Storage.prototype, method, { configurable: true, value: function (this: Storage, ...args: string[]) {
				probe.writes.push(`${method}:${args[0] ?? ''}`);
				return Reflect.apply(original, this, args);
			} });
		}
		const broadcast = BroadcastChannel.prototype.postMessage;
		BroadcastChannel.prototype.postMessage = function (message) {
			probe.broadcasts.push(this.name);
			return broadcast.call(this, message);
		};
	}, { endpoint: action.endpoint });
	return { pageErrors, navigations, external };
}

async function seedState(page: Page) {
	await page.evaluate(({ snapshot, context, email }) => {
		for (const storage of [sessionStorage, localStorage]) {
			storage.setItem(context, 'abandonment-account');
			storage.setItem(`${snapshot}.abandonment-account`, JSON.stringify({
				savedAt: '2026-10-19T12:00:00Z', items: [{ id: 'held-track', title: 'Keep this snapshot' }]
			}));
			storage.setItem(snapshot, 'legacy snapshot fixture');
		}
		sessionStorage.setItem(email, 'fixture@example.com');
	}, { snapshot: SNAPSHOT, context: AUTH_CONTEXT, email: EMAIL });
}

async function storedState(page: Page) {
	return page.evaluate(({ snapshot, context, email }) => ({
		areas: [sessionStorage, localStorage].map((storage) => ({
			context: storage.getItem(context), scoped: storage.getItem(`${snapshot}.abandonment-account`), legacy: storage.getItem(snapshot)
		})), email: sessionStorage.getItem(email)
	}), { snapshot: SNAPSHOT, context: AUTH_CONTEXT, email: EMAIL });
}

async function begin(page: Page, action: Action) {
	await page.goto(action.path);
	const button = page.getByRole('button', { name: action.button });
	await expect(button).toBeEnabled();
	await seedState(page);
	// Accessible names change to Starting…/Opening…/Signing out… while busy.
	const element = await button.elementHandle();
	if (!element) throw new Error(`Missing action button: ${action.name}`);
	if (action.confirm) page.once('dialog', (dialog) => dialog.accept());
	await button.click();
	await expect.poll(() => page.evaluate(() => window.__abandonment.requests)).toEqual([
		{ method: 'POST', path: action.endpoint, body: action.body }
	]);
	await expect.poll(() => element.evaluate((node) => (node as HTMLButtonElement).disabled)).toBe(true);
	await element.dispose();
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
	expect(await page.evaluate(() => window.__abandonment.requests.length)).toBe(1);
}

async function release(page: Page, action: Action, status: number) {
	await page.evaluate(({ status, destination }) => {
		window.__abandonment.release(status, status === 200
			? destination.startsWith('https:') ? { url: destination } : { ok: true }
			: { code: status === 401 ? 'unauthorized' : 'internal_error', message: 'held fixture failure' });
	}, { status, destination: action.destination });
}

async function releaseRaw(page: Page, status: number, rawBody: string) {
	await page.evaluate(({ status, rawBody }) => {
		window.__abandonment.releaseRaw(status, rawBody);
	}, { status, rawBody });
}

async function releaseReject(page: Page) {
	await page.evaluate(() => {
		window.__abandonment.releaseReject();
	});
}

for (const action of ACTIONS) {
	// Qualifying security results (200/401) purge even when abandoned, so only
	// the non-qualifying 500 keeps the preservation contract there. Pricing
	// keeps its 401 branch (plan carry to signup); billing keeps 200/500.
	const preservationStatuses = action.path === '/account/security'
		? [500]
		: action.name === 'pricing checkout' ? [200, 500, 401] : [200, 500];
	for (const status of preservationStatuses) {
		test(`${action.name}: abandoned ${status} cannot navigate or mutate replacement state`, async ({ page }) => {
			const observed = await fixtures(page, action);
			await begin(page, action);
			await clientNavigate(page, '/');
			// Remount before release as well: an old continuation must not
			// poison a fresh instance of the same page with errors/busy state.
			await clientNavigate(page, action.path);
			await expect(page.getByRole('button', { name: action.button })).toBeEnabled();
			const before = await storedState(page);
			observed.navigations.length = 0;
			await page.evaluate(() => {
				window.__abandonment.writes.length = 0;
				window.__abandonment.broadcasts.length = 0;
			});
			await release(page, action, status);
			// Bounded negative-observation window, not a request-arrival sleep:
			// arrival and teardown were explicitly awaited before release.
			await page.waitForTimeout(350);
			expect.soft(observed.external, 'no external navigation attempt').toEqual([]);
			expect.soft(observed.navigations, 'no stale client navigation').toEqual([]);
			expect.soft(new URL(page.url()).pathname).toBe(action.path);
			expect.soft(await storedState(page), 'snapshots and signed-in identity survive').toEqual(before);
			const probe = await page.evaluate(() => ({
				writes: window.__abandonment.writes, broadcasts: window.__abandonment.broadcasts,
				rejections: window.__abandonment.rejections, requests: window.__abandonment.requests
			}));
			expect.soft(probe.writes, 'no stale storage write, including clear-and-restore').toEqual([]);
			expect.soft(probe.broadcasts, 'no stale cross-tab cache invalidation').toEqual([]);
			expect.soft(probe.rejections).toEqual([]);
			expect.soft(observed.pageErrors).toEqual([]);
			expect.soft(probe.requests).toHaveLength(1);
			await expect.soft(page.getByRole('alert')).toHaveCount(0);
			if (new URL(page.url()).pathname === action.path) {
				await expect(page.getByRole('button', { name: action.button })).toBeEnabled();
			}
		});
	}

	test(`${action.name}: live success still completes the held action`, async ({ page }) => {
		const observed = await fixtures(page, action, true);
		await begin(page, action);
		await release(page, action, 200);
		await expect(page).toHaveURL(action.destination.startsWith('https:') ? action.destination : /\/login$/);
		if (action.path === '/account/security') {
			expect(await storedState(page)).toEqual({
				areas: [{ context: null, scoped: null, legacy: null }, { context: null, scoped: null, legacy: null }], email: null
			});
			expect(await page.evaluate(() => window.__abandonment.rejections)).toEqual([]);
		} else {
			await expect(page.getByRole('heading', { name: 'Local Stripe destination' })).toBeVisible();
			expect(observed.external).toEqual([action.destination]);
		}
		expect(observed.pageErrors).toEqual([]);
	});

	test(`${action.name}: live failure settles busy state and allows a retry`, async ({ page }) => {
		const observed = await fixtures(page, action);
		await begin(page, action);
		const before = await storedState(page);
		await release(page, action, 500);
		await expect(page.getByRole('alert')).toBeVisible();
		await expect(page.getByRole('button', { name: action.button })).toBeEnabled();
		expect(await storedState(page)).toEqual(before);
		expect(new URL(page.url()).pathname).toBe(action.path);
		if (action.confirm) page.once('dialog', (dialog) => dialog.accept());
		await page.getByRole('button', { name: action.button }).click();
		await expect.poll(() => page.evaluate(() => window.__abandonment.requests.length)).toBe(2);
		await release(page, action, 500);
		await expect(page.getByRole('button', { name: action.button })).toBeEnabled();
		expect(await page.evaluate(() => window.__abandonment.rejections)).toEqual([]);
		expect(observed.pageErrors).toEqual([]);
		expect(observed.external).toEqual([]);
	});
}

const SECURITY_ACTIONS = ACTIONS.filter((action) => action.path === '/account/security');
const PURGED_STATE = {
	areas: [
		{ context: null, scoped: null, legacy: null },
		{ context: null, scoped: null, legacy: null }
	],
	email: null
};

for (const action of SECURITY_ACTIONS) {
	for (const status of [200, 401]) {
		test(`${action.name}: abandoned ${status} purges auth state but stays silent`, async ({ page }) => {
			const observed = await fixtures(page, action);
			await begin(page, action);
			await clientNavigate(page, '/');
			// Remount before release as well: the purge must happen, but the
			// fresh instance must show no error, message or busy state.
			await clientNavigate(page, action.path);
			await expect(page.getByRole('button', { name: action.button })).toBeEnabled();
			const seeded = await storedState(page);
			// The seed survived both navigations: nothing purges on unmount.
			expect(seeded.email).toBe('fixture@example.com');
			expect(seeded.areas[0].context).toBe('abandonment-account');
			observed.navigations.length = 0;
			await page.evaluate(() => {
				window.__abandonment.writes.length = 0;
				window.__abandonment.broadcasts.length = 0;
			});
			await release(page, action, status);
			// Bounded negative-observation window, not a request-arrival sleep:
			// arrival and teardown were explicitly awaited before release.
			await page.waitForTimeout(350);
			// The qualifying result crossed the auth boundary: snapshots,
			// context and the per-tab identity label are gone in both areas.
			expect(await storedState(page)).toEqual(PURGED_STATE);
			const probe = await page.evaluate(() => ({
				writes: window.__abandonment.writes, broadcasts: window.__abandonment.broadcasts,
				rejections: window.__abandonment.rejections, requests: window.__abandonment.requests
			}));
			// Purge plus the existing cross-tab transition broadcast happened...
			expect(probe.writes).toContain('removeItem:nntune.library.auth-context.v1');
			expect(probe.writes).toContain('removeItem:nntune:signed-in-email');
			expect(probe.writes).toContain('setItem:nntune.library.auth-transition.v1');
			// ...but the abandoned route wrote no UI, navigation or new transport.
			expect.soft(probe.broadcasts, 'no BroadcastChannel relay').toEqual([]);
			expect.soft(observed.external, 'no external navigation attempt').toEqual([]);
			expect.soft(observed.navigations, 'no stale client navigation').toEqual([]);
			expect.soft(new URL(page.url()).pathname).toBe(action.path);
			expect.soft(probe.rejections).toEqual([]);
			expect.soft(observed.pageErrors).toEqual([]);
			expect.soft(probe.requests, 'exactly one security request, no retry').toHaveLength(1);
			await expect.soft(page.getByRole('alert')).toHaveCount(0);
			if (action.confirm) {
				await expect.soft(page.getByText(/session expired before other devices/i)).toHaveCount(0);
			}
			await expect(page.getByRole('button', { name: action.button })).toBeEnabled();
		});
	}

	test(`${action.name}: abandoned network failure purges nothing and stays silent`, async ({ page }) => {
		const observed = await fixtures(page, action);
		await begin(page, action);
		await clientNavigate(page, '/');
		await clientNavigate(page, action.path);
		await expect(page.getByRole('button', { name: action.button })).toBeEnabled();
		const before = await storedState(page);
		expect(before.email).toBe('fixture@example.com');
		observed.navigations.length = 0;
		await page.evaluate(() => {
			window.__abandonment.writes.length = 0;
			window.__abandonment.broadcasts.length = 0;
		});
		await releaseReject(page);
		await page.waitForTimeout(350);
		// A transport failure confirms nothing about the session: no purge,
		// no notification, no navigation, no error UI on the replacement page.
		expect.soft(await storedState(page), 'snapshots and signed-in identity survive').toEqual(before);
		const probe = await page.evaluate(() => ({
			writes: window.__abandonment.writes, broadcasts: window.__abandonment.broadcasts,
			rejections: window.__abandonment.rejections, requests: window.__abandonment.requests
		}));
		expect.soft(probe.writes, 'no stale storage write').toEqual([]);
		expect.soft(probe.broadcasts, 'no stale cross-tab invalidation').toEqual([]);
		expect.soft(observed.navigations, 'no stale client navigation').toEqual([]);
		expect.soft(new URL(page.url()).pathname).toBe(action.path);
		expect.soft(probe.rejections).toEqual([]);
		expect.soft(observed.pageErrors).toEqual([]);
		expect.soft(probe.requests).toHaveLength(1);
		await expect.soft(page.getByRole('alert')).toHaveCount(0);
		await expect(page.getByRole('button', { name: action.button })).toBeEnabled();
	});

	test(`${action.name}: abandoned 200 with a malformed body purges nothing`, async ({ page }) => {
		const observed = await fixtures(page, action);
		await begin(page, action);
		await clientNavigate(page, '/');
		await clientNavigate(page, action.path);
		await expect(page.getByRole('button', { name: action.button })).toBeEnabled();
		const before = await storedState(page);
		expect(before.email).toBe('fixture@example.com');
		observed.navigations.length = 0;
		await page.evaluate(() => {
			window.__abandonment.writes.length = 0;
			window.__abandonment.broadcasts.length = 0;
		});
		// A raw 200 whose body rejects under the parsing contract is not a
		// fulfilled success: it must not purge, notify, navigate or render.
		await releaseRaw(page, 200, 'not-json{{{');
		await page.waitForTimeout(350);
		expect.soft(await storedState(page), 'snapshots and signed-in identity survive').toEqual(before);
		const probe = await page.evaluate(() => ({
			writes: window.__abandonment.writes, broadcasts: window.__abandonment.broadcasts,
			rejections: window.__abandonment.rejections, requests: window.__abandonment.requests
		}));
		expect.soft(probe.writes, 'no stale storage write').toEqual([]);
		expect.soft(probe.broadcasts, 'no stale cross-tab invalidation').toEqual([]);
		expect.soft(observed.navigations, 'no stale client navigation').toEqual([]);
		expect.soft(new URL(page.url()).pathname).toBe(action.path);
		expect.soft(probe.rejections).toEqual([]);
		expect.soft(observed.pageErrors).toEqual([]);
		expect.soft(probe.requests).toHaveLength(1);
		await expect.soft(page.getByRole('alert')).toHaveCount(0);
		await expect(page.getByRole('button', { name: action.button })).toBeEnabled();
	});
}

test('pricing live 401 still carries the selected plan to signup', async ({ page }) => {
	const action = ACTIONS[0];
	const observed = await fixtures(page, action);
	await begin(page, action);
	await release(page, action, 401);
	await expect(page).toHaveURL(/\/signup\?plan=plus$/);
	expect(await page.evaluate(() => window.__abandonment.rejections)).toEqual([]);
	expect(observed.pageErrors).toEqual([]);
});

test('live revoke-all 401 keeps the explicit not-revoked notice and sign-in link', async ({ page }) => {
	const action = ACTIONS[4];
	const observed = await fixtures(page, action);
	await begin(page, action);
	await release(page, action, 401);
	const alert = page.getByRole('alert');
	await expect(alert).toContainText(/session expired before other devices were signed out/i);
	await expect(alert.getByRole('link', { name: /sign in again/i })).toHaveAttribute('href', '/login');
	expect(observed.pageErrors).toEqual([]);
	await expect(page.getByRole('button', { name: action.button })).toBeEnabled();
	expect(new URL(page.url()).pathname).toBe('/account/security');
	expect(await storedState(page)).toEqual({
		areas: [{ context: null, scoped: null, legacy: null }, { context: null, scoped: null, legacy: null }], email: null
	});
	expect(await page.evaluate(() => window.__abandonment.rejections)).toEqual([]);
});
