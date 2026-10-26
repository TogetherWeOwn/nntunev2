import { expect, test, type Page } from '@playwright/test';

test('stale session on provider save directs the user to sign in again', async ({ page }) => {
	await page.route('**/v1/vault/test', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({ ok: true, latencyMs: 12 })
		})
	);
	await page.route('**/v1/vault', (route) =>
		route.fulfill({
			status: 401,
			contentType: 'application/json',
			body: JSON.stringify({
				code: 'reauth_required',
				message: 'reauth required',
				retryable: false
			})
		})
	);
	await page.route('**/v1/session/reauthenticate/start', (route) =>
		route.fulfill({
			status: 401,
			contentType: 'application/json',
			body: JSON.stringify({ error: 'check your email and try again' })
		})
	);

	await page.goto('/onboarding/byop');
	await page.getByLabel('Hostname').fill('news.example.test');
	await page.getByLabel('Username').fill('reader');
	await page.getByLabel('Password').fill('fixture-password');
	await page.getByRole('button', { name: 'Continue', exact: true }).click();
	await page.getByRole('button', { name: 'Continue', exact: true }).click();
	await page.getByTestId('byop-test-connection').click();
	await expect(page.getByTestId('byop-save')).toBeVisible();
	await page.getByTestId('byop-save').click();

	const recovery = page.getByRole('alert');
	await expect(recovery).toContainText('Your session is too old to save provider details.');
	await expect(recovery).toContainText('Reauthenticate with your passkey to continue.');
	await recovery.getByRole('button', { name: 'Reauthenticate and save' }).click();
	await expect(recovery.getByRole('link', { name: 'Sign in again' })).toHaveAttribute(
		'href',
		'/login'
	);
	await expect(page.getByText('Could not save. Your details were not stored — try again.')).toHaveCount(0);
});

test('successful passkey reauthentication saves once with controls disabled while pending', async ({ page }) => {
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
		Object.defineProperty(navigator, 'credentials', {
			value: { get: async () => new FixturePublicKeyCredential() }
		});
	});
	await page.route('**/v1/vault/test', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({ ok: true, latencyMs: 12 })
		})
	);
	let vaultSaves = 0;
	const saveBodies: Array<Record<string, unknown>> = [];
	let releaseSecondSave: (() => void) | undefined;
	let resolveSecondSaveStarted: (() => void) | undefined;
	const secondSaveStarted = new Promise<void>((resolve) => {
		resolveSecondSaveStarted = resolve;
	});
	await page.route('**/v1/vault', async (route) => {
		vaultSaves += 1;
		saveBodies.push(route.request().postDataJSON() as Record<string, unknown>);
		if (vaultSaves === 1) {
			await route.fulfill({
				status: 401,
				contentType: 'application/json',
				body: JSON.stringify({ code: 'reauth_required', message: 'reauth required', retryable: false })
			});
			return;
		}
		resolveSecondSaveStarted?.();
		await new Promise<void>((resolve) => {
			releaseSecondSave = resolve;
		});
		await route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({ ok: true })
		});
	});
	await page.route('**/v1/session/reauthenticate/start', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({ challenge: 'Y2hhbGxlbmdl', rp_id: 'localhost' })
		})
	);
	await page.route('**/v1/session/reauthenticate/finish', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({ ok: true })
		})
	);

	await page.goto('/onboarding/byop');
	await page.getByLabel('Hostname').fill('news.example.test');
	await page.getByLabel('Username').fill('reader');
	await page.getByLabel('Password').fill('fixture-password');
	await page.getByRole('button', { name: 'Continue', exact: true }).click();
	await page.getByRole('button', { name: 'Continue', exact: true }).click();
	await page.getByTestId('byop-test-connection').click();
	await expect(page.getByTestId('byop-save')).toBeVisible();
	await page.getByTestId('byop-save').click();
	await expect(page.getByRole('alert')).toContainText('Reauthenticate with your passkey to continue.');
	await page.getByRole('button', { name: 'Reauthenticate and save' }).click();

	await secondSaveStarted;
	expect(vaultSaves).toBe(2);
	expect(saveBodies[1]).toEqual(saveBodies[0]);
	await expect(page.getByTestId('byop-save')).toBeDisabled();
	await expect(page.getByRole('button', { name: 'Back' })).toBeDisabled();

	releaseSecondSave?.();
	await page.waitForURL('/library');
	expect(vaultSaves).toBe(2);
});

const TRANSITION_RESTART_NOTICE =
	'This setup was reset after an authentication change. Sign in and start setup again; ' +
	're-enter and retest your credentials. Requests already submitted may still complete.';

interface HeldEndpoints {
	release: (path: string, status: number, body: unknown) => Promise<void>;
	count: (path: string) => Promise<number>;
}

/**
 * Hold API endpoints in-page so a late response still arrives even when
 * the page aborts its own request on reset: the fence, not the transport,
 * must suppress it.
 */
async function holdEndpoints(page: Page, paths: string[]): Promise<HeldEndpoints> {
	await page.addInitScript((held) => {
		const nativeFetch = window.fetch.bind(window);
		const hooks = window as unknown as {
			__heldCounts: Record<string, number>;
			__settleHeld: Record<string, (response: Response) => void>;
		};
		hooks.__heldCounts = {};
		hooks.__settleHeld = {};
		window.fetch = (input, init) => {
			const request = new Request(input, init);
			const pathname = new URL(request.url).pathname;
			if (!(held as string[]).includes(pathname)) return nativeFetch(input, init);
			return new Promise<Response>((resolve) => {
				hooks.__heldCounts[pathname] = (hooks.__heldCounts[pathname] ?? 0) + 1;
				hooks.__settleHeld[pathname] = (response) => resolve(response);
			});
		};
	}, paths);
	return {
		release: (path, status, body) =>
			page.evaluate(
				({ path, status, body }) => {
					const settle = (window as unknown as { __settleHeld: Record<string, (r: Response) => void> })
						.__settleHeld[path];
					if (!settle) throw new Error(`No held request for ${path}`);
					settle(
						new Response(JSON.stringify(body), {
							status,
							headers: { 'content-type': 'application/json' }
						})
					);
				},
				{ path, status, body }
			),
		count: (path) =>
			page.evaluate(
				(wanted) =>
					(window as unknown as { __heldCounts?: Record<string, number> }).__heldCounts?.[
						wanted
					] ?? 0,
				path
			)
	};
}

/** Resolve every ceremony immediately with a fixture credential. */
async function installImmediateCeremony(page: Page): Promise<void> {
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
		Object.defineProperty(navigator, 'credentials', {
			value: { get: async () => new FixturePublicKeyCredential() }
		});
	});
}

/**
 * Hold the browser ceremony open until its abort signal fires, like the
 * native passkey prompt the page drives. Signal presence is recorded in
 * `window.__ceremonyGotSignal` and the settlement in
 * `window.__ceremonyOutcome` (`pending` until the signal aborts); calls
 * are counted in `window.__ceremonyCalls`.
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
			__ceremonyGotSignal: boolean;
			__ceremonyOutcome: 'pending' | 'aborted';
		};
		hooks.__ceremonyCalls = 0;
		hooks.__ceremonyGotSignal = false;
		hooks.__ceremonyOutcome = 'pending';
		Object.defineProperty(navigator, 'credentials', {
			value: {
				get: async (options?: { signal?: AbortSignal }) => {
					hooks.__ceremonyCalls += 1;
					const signal = options?.signal;
					// Record signal presence before abort settlement so the
					// no-signal mutant stays observable even though nothing
					// ever aborts it.
					if (signal instanceof AbortSignal) hooks.__ceremonyGotSignal = true;
					if (signal?.aborted) {
						hooks.__ceremonyOutcome = 'aborted';
						throw new DOMException('fixture ceremony aborted', 'AbortError');
					}
					await new Promise<void>((_resolve, reject) => {
						signal?.addEventListener(
							'abort',
							() => {
								hooks.__ceremonyOutcome = 'aborted';
								reject(new DOMException('fixture ceremony aborted', 'AbortError'));
							},
							{ once: true }
						);
					});
					return new FixturePublicKeyCredential();
				}
			}
		});
	});
}

async function ceremonyCalls(page: Page): Promise<number> {
	return page.evaluate(
		() => (window as unknown as { __ceremonyCalls?: number }).__ceremonyCalls ?? -1
	);
}

async function ceremonyGotSignal(page: Page): Promise<boolean> {
	return page.evaluate(
		() => (window as unknown as { __ceremonyGotSignal?: boolean }).__ceremonyGotSignal ?? false
	);
}

async function ceremonyOutcome(page: Page): Promise<string> {
	return page.evaluate(
		() => (window as unknown as { __ceremonyOutcome?: string }).__ceremonyOutcome ?? 'missing'
	);
}

interface ReauthTraffic {
	saves: number;
	startPosts: number;
	finishPosts: number;
}

/**
 * Route the wizard so the connection test passes, the first save demands
 * passkey reauthentication, and later saves succeed; drive to the
 * "Reauthenticate and save" recovery prompt. Start/finish POSTs are
 * counted here only when they are NOT held by `holdEndpoints`.
 */
async function reachReauthPrompt(page: Page): Promise<ReauthTraffic> {
	const traffic: ReauthTraffic = { saves: 0, startPosts: 0, finishPosts: 0 };
	await page.route('**/v1/vault/test', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({ ok: true, latencyMs: 12 })
		})
	);
	await page.route('**/v1/vault', async (route) => {
		traffic.saves += 1;
		const first = traffic.saves === 1;
		await route.fulfill({
			status: first ? 401 : 200,
			contentType: 'application/json',
			body: JSON.stringify(
				first
					? { code: 'reauth_required', message: 'reauth required', retryable: false }
					: { ok: true }
			)
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
	await page.getByLabel('Password', { exact: true }).fill('fixture-password');
	await page.getByRole('button', { name: 'Continue', exact: true }).click();
	await page.getByRole('button', { name: 'Continue', exact: true }).click();
	await page.getByTestId('byop-test-connection').click();
	await expect(page.getByTestId('byop-save')).toBeVisible();
	await page.getByTestId('byop-save').click();
	await expect(page.getByRole('alert')).toContainText(
		'Reauthenticate with your passkey to continue.'
	);
	expect(traffic.saves).toBe(1);
	return traffic;
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

test('transition during held reauth start opens no ceremony and replays no save', async ({
	page
}) => {
	const pageErrors: string[] = [];
	page.on('pageerror', (error) => pageErrors.push(error.message));
	await installImmediateCeremony(page);
	const held = await holdEndpoints(page, ['/v1/session/reauthenticate/start']);
	const traffic = await reachReauthPrompt(page);
	await page.getByRole('button', { name: 'Reauthenticate and save' }).click();
	await expect.poll(() => held.count('/v1/session/reauthenticate/start')).toBe(1);

	await deliverTransition(page);

	const notice = page.getByText(TRANSITION_RESTART_NOTICE);
	await expect(notice).toBeVisible();
	await expect(page.locator('a[href="/login"]')).toHaveCount(1);
	await expect(page.getByRole('button', { name: 'Reauthenticate and save' })).toHaveCount(0);
	await expect(page.getByLabel('Password', { exact: true })).toHaveValue('');

	// The late start response dispatches no ceremony, no finish, no save.
	await held.release('/v1/session/reauthenticate/start', 200, {
		challenge: 'Y2hhbGxlbmdl',
		rp_id: 'localhost'
	});
	await page.waitForTimeout(350);
	await expect(notice).toBeVisible();
	expect(traffic.finishPosts).toBe(0);
	expect(traffic.saves).toBe(1);
	expect(await held.count('/v1/session/reauthenticate/start')).toBe(1);
	expect.soft(new URL(page.url()).pathname).toBe('/onboarding/byop');
	expect.soft(pageErrors).toEqual([]);
});

test('transition during held reauth start suppresses the late limit paywall', async ({
	page
}) => {
	const pageErrors: string[] = [];
	page.on('pageerror', (error) => pageErrors.push(error.message));
	await installImmediateCeremony(page);
	const held = await holdEndpoints(page, ['/v1/session/reauthenticate/start']);
	const traffic = await reachReauthPrompt(page);
	await page.getByRole('button', { name: 'Reauthenticate and save' }).click();
	await expect.poll(() => held.count('/v1/session/reauthenticate/start')).toBe(1);

	await deliverTransition(page);

	const notice = page.getByText(TRANSITION_RESTART_NOTICE);
	await expect(notice).toBeVisible();
	await expect(page.getByRole('button', { name: 'Reauthenticate and save' })).toHaveCount(0);
	await expect(page.getByLabel('Password', { exact: true })).toHaveValue('');

	// The late limit response must not publish the global paywall: the
	// reset aborted the caller's signal and the client checks it before
	// publication. The held transport deliberately ignores abort.
	await held.release('/v1/session/reauthenticate/start', 429, {
		code: 'rate_limited',
		message: 'limit reached'
	});
	await page.waitForTimeout(350);
	await expect(page.getByRole('dialog')).toHaveCount(0);
	await expect(notice).toBeVisible();
	expect(traffic.finishPosts).toBe(0);
	expect(traffic.saves).toBe(1);
	expect(await held.count('/v1/session/reauthenticate/start')).toBe(1);
	expect.soft(new URL(page.url()).pathname).toBe('/onboarding/byop');
	expect.soft(pageErrors).toEqual([]);
});

test('transition during the held ceremony aborts it and replays no save', async ({
	page
}) => {
	const pageErrors: string[] = [];
	page.on('pageerror', (error) => pageErrors.push(error.message));
	await installHeldCeremony(page);
	const traffic = await reachReauthPrompt(page);
	await page.getByRole('button', { name: 'Reauthenticate and save' }).click();
	await expect.poll(() => ceremonyCalls(page)).toBe(1);
	expect(traffic.startPosts).toBe(1);

	await deliverTransition(page);

	const notice = page.getByText(TRANSITION_RESTART_NOTICE);
	await expect(notice).toBeVisible();
	await expect(page.getByRole('button', { name: 'Reauthenticate and save' })).toHaveCount(0);

	// The reset aborts the pending ceremony in place — the test never
	// releases it — so no finish and no save can follow. The ceremony must
	// have been handed an abort signal for that to happen.
	await expect.poll(() => ceremonyOutcome(page)).toBe('aborted');
	expect(await ceremonyGotSignal(page)).toBe(true);
	await page.waitForTimeout(350);
	await expect(notice).toBeVisible();
	expect(await ceremonyCalls(page)).toBe(1);
	expect(traffic.finishPosts).toBe(0);
	expect(traffic.saves).toBe(1);
	expect.soft(new URL(page.url()).pathname).toBe('/onboarding/byop');
	expect.soft(pageErrors).toEqual([]);
});

test('transition during held reauth finish replays no save', async ({ page }) => {
	const pageErrors: string[] = [];
	page.on('pageerror', (error) => pageErrors.push(error.message));
	await installImmediateCeremony(page);
	const held = await holdEndpoints(page, ['/v1/session/reauthenticate/finish']);
	const traffic = await reachReauthPrompt(page);
	await page.getByRole('button', { name: 'Reauthenticate and save' }).click();
	await expect.poll(() => held.count('/v1/session/reauthenticate/finish')).toBe(1);
	expect(traffic.startPosts).toBe(1);

	await deliverTransition(page);

	const notice = page.getByText(TRANSITION_RESTART_NOTICE);
	await expect(notice).toBeVisible();
	await expect(page.getByRole('button', { name: 'Reauthenticate and save' })).toHaveCount(0);

	// The late finish success replays no save, restores nothing, navigates
	// nowhere: an old reauth success must never replay a save.
	await held.release('/v1/session/reauthenticate/finish', 200, { ok: true });
	await page.waitForTimeout(350);
	await expect(notice).toBeVisible();
	expect(traffic.saves).toBe(1);
	expect(await held.count('/v1/session/reauthenticate/finish')).toBe(1);
	await expect(page.getByText(/credentials are sealed/i)).toHaveCount(0);
	expect.soft(new URL(page.url()).pathname).toBe('/onboarding/byop');
	expect.soft(pageErrors).toEqual([]);
});

test('transition during held reauth finish suppresses the late limit paywall', async ({
	page
}) => {
	const pageErrors: string[] = [];
	page.on('pageerror', (error) => pageErrors.push(error.message));
	await installImmediateCeremony(page);
	const held = await holdEndpoints(page, ['/v1/session/reauthenticate/finish']);
	const traffic = await reachReauthPrompt(page);
	await page.getByRole('button', { name: 'Reauthenticate and save' }).click();
	await expect.poll(() => held.count('/v1/session/reauthenticate/finish')).toBe(1);
	expect(traffic.startPosts).toBe(1);

	await deliverTransition(page);

	const notice = page.getByText(TRANSITION_RESTART_NOTICE);
	await expect(notice).toBeVisible();
	await expect(page.getByRole('button', { name: 'Reauthenticate and save' })).toHaveCount(0);

	// The late limit response must not publish the global paywall: the
	// reset aborted the caller's signal and the client checks it before
	// publication. The held transport deliberately ignores abort.
	await held.release('/v1/session/reauthenticate/finish', 429, {
		code: 'rate_limited',
		message: 'limit reached'
	});
	await page.waitForTimeout(350);
	await expect(page.getByRole('dialog')).toHaveCount(0);
	await expect(notice).toBeVisible();
	expect(traffic.saves).toBe(1);
	expect(await held.count('/v1/session/reauthenticate/finish')).toBe(1);
	await expect(page.getByText(/credentials are sealed/i)).toHaveCount(0);
	expect.soft(new URL(page.url()).pathname).toBe('/onboarding/byop');
	expect.soft(pageErrors).toEqual([]);
});

test('leaving during held reauth finish finishes nothing when it resolves', async ({ page }) => {
	const pageErrors: string[] = [];
	page.on('pageerror', (error) => pageErrors.push(error.message));
	await page.addInitScript(() => {
		sessionStorage.setItem('nntune:signed-in-email', 'reader@example.test');
	});
	await installImmediateCeremony(page);
	const held = await holdEndpoints(page, ['/v1/session/reauthenticate/finish']);
	const traffic = await reachReauthPrompt(page);
	await page.getByRole('button', { name: 'Reauthenticate and save' }).click();
	await expect.poll(() => held.count('/v1/session/reauthenticate/finish')).toBe(1);

	// In-app navigation destroys the wizard page while the finish stays held
	// open in the persisting realm.
	await page.getByRole('link', { name: 'sign in again' }).click();
	await page.waitForURL('**/login');
	await expect(page.getByRole('heading', { name: 'Sign in' })).toBeVisible();

	// The late finish resolution must finish nothing: no second save, no
	// hijack to /library.
	await held.release('/v1/session/reauthenticate/finish', 200, { ok: true });
	await page.waitForTimeout(350);
	await expect(page).toHaveURL(/\/login/);
	expect(traffic.saves).toBe(1);
	expect(await held.count('/v1/session/reauthenticate/finish')).toBe(1);
	expect.soft(pageErrors).toEqual([]);
});
