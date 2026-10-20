import { expect, test, type Page } from '@playwright/test';

/**
 * Regression: leaving the BYOP wizard mid-operation must not let the late
 * continuation hijack the new route.
 *
 * `onDestroy` in `src/routes/onboarding/byop/+page.svelte` marks the page
 * disposed and invalidates the test generation. Navigating away through
 * an in-app link (client-side navigation: the component is destroyed but
 * the JS realm persists) while a save PUT, reauthentication start POST or
 * ceremony is held open must settle to nothing: releasing the operation keeps
 * the browser on the new route with no redirect to /library and no
 * further ceremony/finish/save traffic.
 *
 * All API responses and `navigator.credentials` are synthetic fixtures
 * with controlled (held) promises; no live provider or authenticator.
 */

type JsonBody = Record<string, unknown>;

interface Traffic {
	testBodies: JsonBody[];
	saveBodies: JsonBody[];
	startPosts: number;
	finishPosts: number;
}

/** A promise released from test code, for holding a route open. */
function held(): { promise: Promise<void>; release: () => void } {
	let release!: () => void;
	const promise = new Promise<void>((resolve) => {
		release = resolve;
	});
	return { promise, release };
}

const TEST_OK = { status: 200, body: { ok: true, latencyMs: 12 } };

interface RouteOptions {
	/** Response for the first vault save (default: 200 ok). */
	firstSave?: { status: number; body: JsonBody };
	/** Response for later vault saves (default: same as firstSave). */
	nextSave?: { status: number; body: JsonBody };
	/** Hold the first vault save until `releaseFirstSave()`. */
	holdFirstSave?: boolean;
	/** Hold the reauthentication start response until `releaseStart()`. */
	holdStart?: boolean;
}

interface Wizard {
	traffic: Traffic;
	releaseFirstSave: () => void;
	firstSaveStarted: Promise<void>;
	releaseStart: () => void;
	startStarted: Promise<void>;
}

/**
 * Show the signed-in identity banner (which carries an in-app link to
 * /login), route the provider endpoints, then drive the wizard to step 3
 * with a passing connection test.
 */
async function reachTestedConfig(page: Page, options: RouteOptions = {}): Promise<Wizard> {
	const traffic: Traffic = { testBodies: [], saveBodies: [], startPosts: 0, finishPosts: 0 };
	const saveHold = held();
	const firstSave = held();
	const startHold = held();
	const start = held();
	const firstSaveResponse = options.firstSave ?? { status: 200, body: { ok: true } };
	const nextSaveResponse = options.nextSave ?? firstSaveResponse;

	await page.route('**/v1/vault/test', async (route) => {
		traffic.testBodies.push(route.request().postDataJSON() as JsonBody);
		await route.fulfill({
			status: TEST_OK.status,
			contentType: 'application/json',
			body: JSON.stringify(TEST_OK.body)
		});
	});
	await page.route('**/v1/vault', async (route) => {
		traffic.saveBodies.push(route.request().postDataJSON() as JsonBody);
		const isFirst = traffic.saveBodies.length === 1;
		if (isFirst) {
			firstSave.release();
			if (options.holdFirstSave) await saveHold.promise;
		}
		const response = isFirst ? firstSaveResponse : nextSaveResponse;
		await route.fulfill({
			status: response.status,
			contentType: 'application/json',
			body: JSON.stringify(response.body)
		});
	});
	await page.route('**/v1/session/reauthenticate/start', async (route) => {
		traffic.startPosts += 1;
		start.release();
		if (options.holdStart) await startHold.promise;
		await route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({ challenge: 'Y2hhbGxlbmdl', rp_id: 'localhost' })
		});
	});
	await page.route('**/v1/session/reauthenticate/finish', async (route) => {
		traffic.finishPosts += 1;
		await route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({ ok: true })
		});
	});

	await page.goto('/onboarding/byop');
	await page.getByLabel('Hostname').fill('news.example.test');
	await page.getByLabel('Username').fill('reader');
	await page.getByLabel('Password').fill('fixture-password');
	await page.getByRole('button', { name: 'Continue' }).click();
	await page.getByRole('button', { name: 'Continue' }).click();
	await page.getByTestId('byop-test-connection').click();
	await expect(page.getByTestId('byop-save')).toBeVisible();

	return {
		traffic,
		releaseFirstSave: saveHold.release,
		firstSaveStarted: firstSave.promise,
		releaseStart: startHold.release,
		startStarted: start.promise
	};
}

/** Show the signed-in banner so an in-app link is available mid-operation. */
async function showIdentityBanner(page: Page): Promise<void> {
	await page.addInitScript(() => {
		sessionStorage.setItem('nntune:signed-in-email', 'reader@example.test');
	});
}

/**
 * Install a `navigator.credentials.get` double whose ceremony stays
 * open until the test calls `window.__releaseCeremony()`. The call
 * count is exposed as `window.__ceremonyCalls`.
 */
async function installHeldCeremony(page: Page): Promise<void> {
	await page.addInitScript(() => {
		class FixturePublicKeyCredential {
			id = 'fixture-credential';
			rawId = new Uint8Array([1]).buffer;
			type = 'public-key';
			authenticatorAttachment = 'platform';
			response = {
				clientDataJSON: new Uint8Array([2]).buffer,
				authenticatorData: new Uint8Array([3]).buffer,
				signature: new Uint8Array([4]).buffer,
				userHandle: null
			};
			getClientExtensionResults() {
				return {};
			}
		}
		Object.defineProperty(window, 'PublicKeyCredential', {
			value: FixturePublicKeyCredential
		});
		const hooks = window as unknown as {
			__ceremonyCalls: number;
			__releaseCeremony: () => void;
			__resolveCeremony?: () => void;
		};
		hooks.__ceremonyCalls = 0;
		hooks.__releaseCeremony = () => hooks.__resolveCeremony?.();
		Object.defineProperty(navigator, 'credentials', {
			value: {
				get: async () => {
					hooks.__ceremonyCalls += 1;
					await new Promise<void>((resolve) => {
						hooks.__resolveCeremony = resolve;
					});
					return new FixturePublicKeyCredential();
				}
			}
		});
	});
}

async function releaseCeremony(page: Page): Promise<void> {
	await page.evaluate(() => {
		(window as unknown as { __releaseCeremony?: () => void }).__releaseCeremony?.();
	});
}

async function ceremonyCalls(page: Page): Promise<number> {
	return page.evaluate(
		() => (window as unknown as { __ceremonyCalls?: number }).__ceremonyCalls ?? -1
	);
}

test('leaving mid-save keeps the new route when the PUT resolves', async ({ page }) => {
	await showIdentityBanner(page);
	const wizard = await reachTestedConfig(page, { holdFirstSave: true });

	await page.getByTestId('byop-save').click();
	await wizard.firstSaveStarted;
	await expect(page.getByTestId('byop-save')).toHaveText(/Saving/);

	// In-app navigation destroys the wizard page while the JS realm (and
	// the held PUT promise) persists.
	await page.getByRole('link', { name: 'sign in again' }).click();
	await page.waitForURL('**/login');
	await expect(page.getByRole('heading', { name: 'Sign in' })).toBeVisible();

	// The late PUT resolution must settle to nothing: no hijack to
	// /library, no duplicate save.
	wizard.releaseFirstSave();
	await page.waitForTimeout(500);
	await expect(page).toHaveURL(/\/login/);
	expect(wizard.traffic.saveBodies).toHaveLength(1);
	expect(wizard.traffic.finishPosts).toBe(0);
});

test('leaving mid-reauthentication start opens no ceremony when the POST resolves', async ({ page }) => {
	await showIdentityBanner(page);
	await installHeldCeremony(page);
	const wizard = await reachTestedConfig(page, {
		firstSave: {
			status: 401,
			body: { code: 'reauth_required', message: 'reauth required', retryable: false }
		},
		nextSave: { status: 200, body: { ok: true } },
		holdStart: true
	});

	await page.getByTestId('byop-save').click();
	await expect(page.getByRole('alert')).toContainText(
		'Reauthenticate with your passkey to continue.'
	);
	await page.getByRole('button', { name: 'Reauthenticate and save' }).click();
	await wizard.startStarted;
	expect(await ceremonyCalls(page)).toBe(0);

	// Destroy the component before start returns, not after a ceremony is
	// already open. The persisting realm must never open that late prompt.
	await page.getByRole('link', { name: 'sign in again' }).click();
	await page.waitForURL('**/login');
	await expect(page.getByRole('heading', { name: 'Sign in' })).toBeVisible();

	const startResponse = page.waitForResponse('**/v1/session/reauthenticate/start');
	wizard.releaseStart();
	expect(await (await startResponse).finished()).toBeNull();
	await page.waitForTimeout(500);
	expect(await ceremonyCalls(page)).toBe(0);
	await expect(page).toHaveURL(/\/login/);
	expect(wizard.traffic.startPosts).toBe(1);
	expect(wizard.traffic.finishPosts).toBe(0);
	expect(wizard.traffic.saveBodies).toHaveLength(1);
});

test('leaving mid-ceremony finishes nothing when the ceremony resolves', async ({ page }) => {
	await showIdentityBanner(page);
	await installHeldCeremony(page);
	const wizard = await reachTestedConfig(page, {
		firstSave: {
			status: 401,
			body: { code: 'reauth_required', message: 'reauth required', retryable: false }
		},
		nextSave: { status: 200, body: { ok: true } }
	});

	await page.getByTestId('byop-save').click();
	await expect(page.getByRole('alert')).toContainText(
		'Reauthenticate with your passkey to continue.'
	);
	await page.getByRole('button', { name: 'Reauthenticate and save' }).click();
	await expect.poll(() => ceremonyCalls(page)).toBe(1);

	// In-app navigation destroys the wizard page while the ceremony stays
	// held open in the persisting realm.
	await page.getByRole('link', { name: 'sign in again' }).click();
	await page.waitForURL('**/login');
	await expect(page.getByRole('heading', { name: 'Sign in' })).toBeVisible();

	// The late ceremony resolution must finish nothing: no reauth finish,
	// no second save, no hijack to /library.
	await releaseCeremony(page);
	await page.waitForTimeout(500);
	await expect(page).toHaveURL(/\/login/);
	expect(await ceremonyCalls(page)).toBe(1);
	expect(wizard.traffic.finishPosts).toBe(0);
	expect(wizard.traffic.saveBodies).toHaveLength(1);
});
