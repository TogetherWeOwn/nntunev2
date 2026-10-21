import { expect, test, type Page } from '@playwright/test';

/**
 * Acceptance: BYOP connection test, provider save and passkey
 * reauthentication must be mutually exclusive.
 *
 * In `src/routes/onboarding/byop/+page.svelte`, `runTest` increments
 * `testRunId` on every new test, while `save` snapshots that id before
 * its PUT and only settles `saveState` when the id still matches. The
 * "Test connection" button is disabled only while a test is running, so
 * a retest attempted during an in-flight save invalidates the save's
 * generation: the save continuation then returns without settling and
 * the wizard stays "Saving…" forever with Back disabled. `runTest` also
 * has no handler-entry guard, so programmatic entry during a save or a
 * held reauthentication ceremony starts an overlapping test and spoils
 * the in-flight operation (a reauth in progress then fails its
 * tested-configuration check and reports a bogus failure).
 *
 * The contract pinned here:
 *
 * - While a save PUT is in flight, Test is disabled in the visible
 *   controls AND refused at handler entry (synthetic `click` dispatch,
 *   which bypasses `disabled`); releasing the PUT settles the save —
 *   success redirects to the library with the exact tested payload, a
 *   `reauth_required` refusal settles to the reauthentication prompt —
 *   never a permanent "Saving…".
 * - While a reauthentication ceremony is held open, Test, Save and
 *   Back are all refused (visibly and at handler entry) and a repeated
 *   reauthentication entry starts no second ceremony; releasing the
 *   ceremony finishes and saves exactly once.
 * - Settle-and-retry controls stay intact: an ordinary save failure
 *   and a paywalled save both leave the wizard retryable, and editing
 *   after a passing test still voids the test-before-save binding.
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
}

interface Wizard {
	traffic: Traffic;
	releaseFirstSave: () => void;
	firstSaveStarted: Promise<void>;
}

/**
 * Route the provider test, vault save and reauthentication endpoints,
 * then drive the wizard to step 3 with a passing connection test.
 */
async function reachTestedConfig(page: Page, options: RouteOptions = {}): Promise<Wizard> {
	const traffic: Traffic = { testBodies: [], saveBodies: [], startPosts: 0, finishPosts: 0 };
	const saveHold = held();
	const firstSave = held();
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
		firstSaveStarted: firstSave.promise
	};
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

test('an in-flight save blocks retest and still settles on success', async ({ page }) => {
	const wizard = await reachTestedConfig(page, { holdFirstSave: true });

	await page.getByTestId('byop-save').click();
	await wizard.firstSaveStarted;
	await expect(page.getByTestId('byop-save')).toHaveText(/Saving/);

	// Visible mutual exclusion: while the PUT is held, no new test can
	// be started from the controls.
	await expect(page.getByTestId('byop-test-connection')).toBeDisabled();
	await expect(page.getByRole('button', { name: 'Back' })).toBeDisabled();

	// Handler-entry mutual exclusion: synthetic clicks (which bypass
	// `disabled`) must not start a second test or a duplicate save.
	await page.getByTestId('byop-test-connection').dispatchEvent('click');
	await page.getByTestId('byop-save').dispatchEvent('click');
	expect(wizard.traffic.testBodies).toHaveLength(1);
	expect(wizard.traffic.saveBodies).toHaveLength(1);

	// Releasing the PUT settles the save: the exact tested payload was
	// saved and the wizard moves on instead of sticking on "Saving…".
	wizard.releaseFirstSave();
	await page.waitForURL('/library');
	expect(wizard.traffic.saveBodies[0]).toEqual(wizard.traffic.testBodies[0]);
});

test('an in-flight save refused for reauthentication settles to the reauth prompt after an attempted retest', async ({
	page
}) => {
	const wizard = await reachTestedConfig(page, {
		holdFirstSave: true,
		firstSave: {
			status: 401,
			body: { code: 'reauth_required', message: 'reauth required', retryable: false }
		}
	});

	await page.getByTestId('byop-save').click();
	await wizard.firstSaveStarted;

	// An attempted retest during the held PUT must not start a test or
	// invalidate the save generation.
	await expect(page.getByTestId('byop-test-connection')).toBeDisabled();
	await page.getByTestId('byop-test-connection').dispatchEvent('click');
	expect(wizard.traffic.testBodies).toHaveLength(1);

	// The refusal settles the busy state into the explicit, retryable
	// reauthentication prompt — never a permanent "Saving…".
	wizard.releaseFirstSave();
	await expect(page.getByRole('alert')).toContainText(
		'Reauthenticate with your passkey to continue.'
	);
	await expect(page.getByRole('button', { name: 'Reauthenticate and save' })).toBeEnabled();
	await expect(page.getByTestId('byop-save')).toBeEnabled();
	await expect(page.getByTestId('byop-save')).not.toHaveText(/Saving/);
	await expect(page.getByRole('button', { name: 'Back' })).toBeEnabled();
	expect(wizard.traffic.saveBodies).toHaveLength(1);
});

test('a held reauthentication ceremony blocks test, save and back, then saves exactly once', async ({
	page
}) => {
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

	// Visible mutual exclusion while the ceremony is held open.
	await expect(page.getByTestId('byop-test-connection')).toBeDisabled();
	await expect(page.getByTestId('byop-save')).toBeDisabled();
	await expect(page.getByRole('button', { name: 'Back' })).toBeDisabled();
	// While working, the reauthentication button shows its busy label.
	const reauthenticate = page.getByRole('button', { name: /Verifying/ });
	await expect(reauthenticate).toBeDisabled();

	// Handler-entry mutual exclusion: synthetic clicks start no test,
	// no duplicate save, no second ceremony and no step change.
	await page.getByTestId('byop-test-connection').dispatchEvent('click');
	await page.getByTestId('byop-save').dispatchEvent('click');
	await page.getByRole('button', { name: 'Back' }).dispatchEvent('click');
	await reauthenticate.dispatchEvent('click');
	expect(wizard.traffic.testBodies).toHaveLength(1);
	expect(wizard.traffic.saveBodies).toHaveLength(1);
	expect(wizard.traffic.startPosts).toBe(1);
	expect(await ceremonyCalls(page)).toBe(1);
	await expect(page.getByRole('heading', { name: 'Test the connection' })).toBeVisible();

	// Releasing the ceremony finishes reauthentication and reuses the
	// save path exactly once, with the exact tested payload.
	await releaseCeremony(page);
	await page.waitForURL('/library');
	expect(wizard.traffic.finishPosts).toBe(1);
	expect(wizard.traffic.saveBodies).toHaveLength(2);
	expect(wizard.traffic.saveBodies[1]).toEqual(wizard.traffic.testBodies[0]);
});

test('an ordinary save failure settles and the same provider can be retried', async ({ page }) => {
	const wizard = await reachTestedConfig(page, {
		firstSave: { status: 500, body: { error: 'upstream unavailable' } },
		nextSave: { status: 200, body: { ok: true } }
	});

	await page.getByTestId('byop-save').click();
	await expect(page.getByRole('alert')).toContainText('Could not save.');
	await expect(page.getByTestId('byop-save')).toBeEnabled();
	await expect(page.getByTestId('byop-save')).not.toHaveText(/Saving/);
	await expect(page.getByTestId('byop-test-connection')).toBeEnabled();
	await expect(page.getByRole('button', { name: 'Back' })).toBeEnabled();

	await page.getByTestId('byop-save').click();
	await page.waitForURL('/library');
	expect(wizard.traffic.saveBodies).toHaveLength(2);
	expect(wizard.traffic.saveBodies[1]).toEqual(wizard.traffic.testBodies[0]);
});

test('a paywalled save settles under the paywall modal and stays retryable', async ({ page }) => {
	const wizard = await reachTestedConfig(page, {
		firstSave: { status: 402, body: { code: 'plan_limit_exceeded' } }
	});

	await page.getByTestId('byop-save').click();
	const dialog = page.getByRole('dialog');
	await expect(dialog).toBeVisible();
	await expect(dialog).toContainText(/reached your plan limit/i);

	await dialog.getByRole('button', { name: /not now/i }).click();
	await expect(dialog).toBeHidden();
	await expect(page.getByTestId('byop-save')).toBeEnabled();
	await expect(page.getByTestId('byop-save')).not.toHaveText(/Saving/);
	await expect(page.getByRole('button', { name: 'Back' })).toBeEnabled();
	expect(wizard.traffic.saveBodies).toHaveLength(1);
});

test('leaving the test step after a passing test still voids the save binding until retested', async ({
	page
}) => {
	const wizard = await reachTestedConfig(page);

	await page.getByRole('button', { name: 'Back' }).click();
	await page.getByRole('button', { name: 'Continue' }).click();

	// Back at the test step, nothing can be saved until the exact
	// current configuration has been tested again.
	await expect(page.getByTestId('byop-save')).toHaveCount(0);
	await page.getByTestId('byop-test-connection').click();
	await expect(page.getByTestId('byop-save')).toBeVisible();
	expect(wizard.traffic.testBodies).toHaveLength(2);
	expect(wizard.traffic.saveBodies).toHaveLength(0);
});
