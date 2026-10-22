import { expect, test, type Page } from '@playwright/test';

/**
 * Acceptance: BYOP passkey reauthentication must not report failure for
 * outcomes that are not failures.
 *
 * `reauthenticateAndSave` in `src/routes/onboarding/byop/+page.svelte`
 * currently funnels every rejection into `reauthState = 'failed'`, which
 * renders "Reauthentication failed. Sign in again to continue." Two
 * outcomes must be told apart from that, matching the paywall-aware
 * catches already used by the connection test and the save on the same
 * page, and the dismissal handling already used by login/signup:
 *
 * - A plan-limit (402/429) response from the reauthenticate start/finish
 *   calls is owned by the global paywall modal. Under the modal there
 *   must be no failure banner, and the reauth control returns to its
 *   idle, retryable state once the modal is dismissed.
 * - `navigator.credentials.get` rejecting with `NotAllowedError` /
 *   `AbortError` means the user dismissed (or timed out) the platform
 *   passkey prompt. That yields a dismissed message — never the
 *   sign-in-again failure — and the ceremony can be retried in place.
 *
 * Genuine reauth failures (a refused finish, a non-dismissal ceremony
 * error) keep the exact current failure banner.
 */

const FAILURE = 'Reauthentication failed';
const DISMISSAL = /dismissed the passkey prompt/i;

/**
 * Install a `navigator.credentials.get` double for the reauth ceremony.
 * Calls listed in `rejectNames` (1-based call index -> error name)
 * reject; every other call resolves with a fixture credential, the same
 * shape `byop-reauth.spec.ts` uses.
 */
async function installGetDouble(page: Page, rejectNames: Record<number, string>): Promise<void> {
	await page.addInitScript((rejects) => {
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
		let calls = 0;
		Object.defineProperty(navigator, 'credentials', {
			value: {
				get: async () => {
					calls += 1;
					const name = (rejects as Record<number, string>)[calls];
					if (name) throw new DOMException('fixture ceremony rejection', name);
					return new FixturePublicKeyCredential();
				}
			}
		});
	}, rejectNames);
}

interface ReauthTraffic {
	vaultSaves: number;
	startPosts: number;
	finishPosts: number;
}

interface ReauthRoutes {
	/** HTTP status for every reauthenticate/start call (default 200). */
	startStatus?: number;
	/** HTTP status for every reauthenticate/finish call (default 200). */
	finishStatus?: number;
}

/**
 * Route the wizard so the first provider save demands reauthentication
 * (401 `reauth_required`) and any later save succeeds, then drive the
 * wizard to the "Reauthenticate and save" recovery prompt.
 */
async function reachReauthPrompt(page: Page, routes: ReauthRoutes = {}): Promise<ReauthTraffic> {
	const traffic: ReauthTraffic = { vaultSaves: 0, startPosts: 0, finishPosts: 0 };
	const startStatus = routes.startStatus ?? 200;
	const finishStatus = routes.finishStatus ?? 200;

	await page.route('**/v1/vault/test', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({ ok: true, latencyMs: 12 })
		})
	);
	await page.route('**/v1/vault', async (route) => {
		traffic.vaultSaves += 1;
		if (traffic.vaultSaves === 1) {
			await route.fulfill({
				status: 401,
				contentType: 'application/json',
				body: JSON.stringify({
					code: 'reauth_required',
					message: 'reauth required',
					retryable: false
				})
			});
			return;
		}
		await route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({ ok: true })
		});
	});
	await page.route('**/v1/session/reauthenticate/start', async (route) => {
		traffic.startPosts += 1;
		await route.fulfill({
			status: startStatus,
			headers: {
				'content-type': 'application/json',
				...(startStatus === 429 ? { 'Retry-After': '45' } : {})
			},
			body: JSON.stringify(
				startStatus === 200
					? { challenge: 'Y2hhbGxlbmdl', rp_id: 'localhost' }
					: startStatus === 429
						? { code: 'rate_limited', message: 'limit reached' }
						: startStatus === 402
							? { code: 'plan_limit_exceeded' }
							: { error: 'check your email and try again' }
			)
		});
	});
	await page.route('**/v1/session/reauthenticate/finish', async (route) => {
		traffic.finishPosts += 1;
		await route.fulfill({
			status: finishStatus,
			headers: {
				'content-type': 'application/json',
				...(finishStatus === 429 ? { 'Retry-After': '45' } : {})
			},
			body: JSON.stringify(
				finishStatus === 200
					? { ok: true }
					: finishStatus === 429
						? { code: 'rate_limited', message: 'limit reached' }
						: finishStatus === 402
							? { code: 'plan_limit_exceeded' }
							: { error: 'check your email and try again' }
			)
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
	await page.getByTestId('byop-save').click();
	await expect(page.getByRole('alert')).toContainText(
		'Reauthenticate with your passkey to continue.'
	);
	return traffic;
}

test('paywalled reauth start returns to the paywall state with no failure banner', async ({
	page
}) => {
	await installGetDouble(page, {});
	const traffic = await reachReauthPrompt(page, { startStatus: 402 });

	await page.getByRole('button', { name: 'Reauthenticate and save' }).click();

	// The global paywall owns the limit response...
	const dialog = page.getByRole('dialog');
	await expect(dialog).toBeVisible();
	await expect(dialog).toContainText(/reached your plan limit/i);
	// ...with no reauth failure reported underneath it, and no
	// sign-in-again dead end.
	await expect(page.getByText(FAILURE)).toHaveCount(0);
	await expect(
		page.getByRole('alert').getByRole('link', { name: 'Sign in again' })
	).toHaveCount(0);
	// The ceremony stopped at the paywalled start: no finish, no retry
	// of the vault save.
	expect(traffic.startPosts).toBe(1);
	expect(traffic.finishPosts).toBe(0);
	expect(traffic.vaultSaves).toBe(1);

	// Dismissing the paywall returns to the recovery prompt in its idle,
	// retryable state — not stuck working, not marked failed.
	await dialog.getByRole('button', { name: /not now/i }).click();
	await expect(dialog).toBeHidden();
	await expect(page.getByText(FAILURE)).toHaveCount(0);
	await expect(page.getByRole('button', { name: 'Reauthenticate and save' })).toBeEnabled();
});

test('paywalled reauth finish shows the paywall with no failure banner', async ({ page }) => {
	await installGetDouble(page, {});
	const traffic = await reachReauthPrompt(page, { finishStatus: 429 });

	await page.getByRole('button', { name: 'Reauthenticate and save' }).click();

	const dialog = page.getByRole('dialog');
	await expect(dialog).toBeVisible();
	await expect(page.getByTestId('paywall-countdown')).toBeVisible();
	await expect(page.getByText(FAILURE)).toHaveCount(0);
	await expect(
		page.getByRole('alert').getByRole('link', { name: 'Sign in again' })
	).toHaveCount(0);
	// The vault save is not retried behind the paywall.
	expect(traffic.startPosts).toBe(1);
	expect(traffic.finishPosts).toBe(1);
	expect(traffic.vaultSaves).toBe(1);
});

for (const errorName of ['NotAllowedError', 'AbortError']) {
	test(`dismissed passkey prompt (${errorName}) yields the dismissed state, not failure`, async ({
		page
	}) => {
		await installGetDouble(page, { 1: errorName });
		const traffic = await reachReauthPrompt(page);

		await page.getByRole('button', { name: 'Reauthenticate and save' }).click();

		const recovery = page.getByRole('alert');
		await expect(page.getByText(DISMISSAL)).toBeVisible();
		await expect(page.getByText(FAILURE)).toHaveCount(0);
		await expect(recovery.getByRole('link', { name: 'Sign in again' })).toHaveCount(0);
		// The recovery prompt stays in place, ready to retry: the button
		// is back to its idle label and enabled.
		await expect(page.getByRole('button', { name: 'Reauthenticate and save' })).toBeEnabled();
		// A dismissed ceremony never reaches the server finish or a save.
		expect(traffic.startPosts).toBe(1);
		expect(traffic.finishPosts).toBe(0);
		expect(traffic.vaultSaves).toBe(1);
	});
}

test('retry after dismissing the passkey prompt reauthenticates and saves', async ({ page }) => {
	await installGetDouble(page, { 1: 'NotAllowedError' });
	const traffic = await reachReauthPrompt(page);

	await page.getByRole('button', { name: 'Reauthenticate and save' }).click();
	await expect(page.getByText(DISMISSAL)).toBeVisible();

	await page.getByRole('button', { name: 'Reauthenticate and save' }).click();
	await page.waitForURL('/library');

	expect(traffic.finishPosts).toBe(1);
	expect(traffic.vaultSaves).toBe(2);
});

test('a genuine reauth finish failure keeps the exact current failure banner', async ({ page }) => {
	await installGetDouble(page, {});
	const traffic = await reachReauthPrompt(page, { finishStatus: 401 });

	await page.getByRole('button', { name: 'Reauthenticate and save' }).click();

	const recovery = page.getByRole('alert');
	await expect(recovery).toContainText('Reauthentication failed.');
	await expect(recovery).not.toContainText(DISMISSAL);
	await expect(recovery.getByRole('link', { name: 'Sign in again' })).toHaveAttribute(
		'href',
		'/login'
	);
	expect(traffic.finishPosts).toBe(1);
	expect(traffic.vaultSaves).toBe(1);
});

test('a non-dismissal ceremony error keeps the exact current failure banner', async ({ page }) => {
	await installGetDouble(page, { 1: 'SecurityError' });
	const traffic = await reachReauthPrompt(page);

	await page.getByRole('button', { name: 'Reauthenticate and save' }).click();

	const recovery = page.getByRole('alert');
	await expect(recovery).toContainText('Reauthentication failed.');
	await expect(recovery).not.toContainText(DISMISSAL);
	await expect(recovery.getByRole('link', { name: 'Sign in again' })).toHaveAttribute(
		'href',
		'/login'
	);
	expect(traffic.finishPosts).toBe(0);
	expect(traffic.vaultSaves).toBe(1);
});

const TRANSITION_RESTART_NOTICE =
	'This setup was reset after an authentication change. Sign in and start setup again; ' +
	're-enter and retest your credentials. Requests already submitted may still complete.';

async function reachDraft(page: Page, tag: string): Promise<void> {
	await page.goto('/onboarding/byop');
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
async function deliverTransition(page: Page): Promise<void> {
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

test('a retained paywall retry is inert after a delivered transition', async ({ page }) => {
	const pageErrors: string[] = [];
	page.on('pageerror', (error) => pageErrors.push(error.message));
	let tests = 0;
	const bodies: Array<Record<string, unknown>> = [];
	await page.route('**/v1/vault/test', async (route) => {
		tests += 1;
		bodies.push(route.request().postDataJSON() as Record<string, unknown>);
		await route.fulfill({
			status: 429,
			headers: { 'content-type': 'application/json', 'Retry-After': '0' },
			body: JSON.stringify({ code: 'rate_limited', message: 'limit reached' })
		});
	});
	await reachDraft(page, 'retry');
	await page.getByTestId('byop-test-connection').click();

	// The limit response publishes the global paywall with a live retained
	// retry; the first dispatch carried the typed secrets.
	const dialog = page.getByRole('dialog');
	await expect(dialog).toBeVisible();
	await expect(dialog.getByRole('button', { name: 'Retry' })).toBeEnabled();
	expect(tests).toBe(1);
	expect(JSON.stringify(bodies[0])).toContain('retry-probe-password');

	await deliverTransition(page);

	// Fail-closed restart; the previously published paywall may remain
	// visible, but its retained retry cannot submit or restore the old draft.
	const notice = page.getByText(TRANSITION_RESTART_NOTICE);
	await expect(notice).toBeVisible();
	await expect(page.locator('a[href="/login"]')).toHaveCount(1);
	if (await dialog.isVisible()) {
		await dialog.getByRole('button', { name: 'Retry' }).click();
	}
	await page.waitForTimeout(350);
	expect(tests).toBe(1);
	expect(bodies).toHaveLength(1);
	await expect(notice).toBeVisible();
	await expect(page.getByTestId('byop-save')).toHaveCount(0);
	expect.soft(new URL(page.url()).pathname).toBe('/onboarding/byop');
	expect.soft(pageErrors).toEqual([]);
});

test('a late limit response after the transition publishes no new paywall', async ({
	page
}) => {
	const pageErrors: string[] = [];
	page.on('pageerror', (error) => pageErrors.push(error.message));
	await page.addInitScript(() => {
		const nativeFetch = window.fetch.bind(window);
		const hooks = window as unknown as {
			__heldTests: number;
			__settleTest?: (response: Response) => void;
		};
		hooks.__heldTests = 0;
		window.fetch = (input, init) => {
			const request = new Request(input, init);
			if (new URL(request.url).pathname !== '/v1/vault/test') {
				return nativeFetch(input, init);
			}
			return new Promise<Response>((resolve) => {
				hooks.__heldTests += 1;
				hooks.__settleTest = (response) => resolve(response);
			});
		};
	});
	await reachDraft(page, 'latepaywall');
	await page.getByTestId('byop-test-connection').click();
	await expect
		.poll(
			() => page.evaluate(() => (window as unknown as { __heldTests: number }).__heldTests ?? -1)
		)
		.toBe(1);

	await deliverTransition(page);

	const notice = page.getByText(TRANSITION_RESTART_NOTICE);
	await expect(notice).toBeVisible();
	await expect(page.locator('a[href="/login"]')).toHaveCount(1);

	// The held limit response arrives after reset: the fenced page must
	// suppress the late global paywall publication, not just the inline
	// state. The held transport deliberately ignores abort.
	await page.evaluate(() => {
		const settle = (window as unknown as { __settleTest?: (r: Response) => void }).__settleTest;
		if (!settle) throw new Error('No held connection test');
		settle(
			new Response(JSON.stringify({ code: 'rate_limited', message: 'limit reached' }), {
				status: 429,
				headers: { 'content-type': 'application/json', 'Retry-After': '0' }
			})
		);
	});
	await page.waitForTimeout(350);
	await expect(page.getByRole('dialog')).toHaveCount(0);
	await expect(notice).toBeVisible();
	await expect(page.getByTestId('byop-save')).toHaveCount(0);
	expect.soft(new URL(page.url()).pathname).toBe('/onboarding/byop');
	expect.soft(pageErrors).toEqual([]);
});
