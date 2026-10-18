import { expect, test, type Page } from '@playwright/test';

/**
 * Acceptance: dismissing the passkey prompt must be told apart from a
 * genuine code/auth failure.
 *
 * `navigator.credentials.create` (signup) and `.get` (login) reject with
 * `NotAllowedError` / `AbortError` when the user cancels or lets the
 * platform prompt time out — before any server finish call. The user
 * must be told they dismissed the prompt (signup: their emailed code is
 * still valid, no fresh code needed), never shown the generic
 * code/sign-in failure that blames the code. Genuine failures and
 * non-dismissal ceremony errors keep their exact current messages, and
 * login stays non-enumerating.
 */

const SIGNUP_GENERIC =
	"That didn't work — re-enter the code (and invite code, if you have one) from the email and try again. If it keeps failing, start over to get a fresh code.";
const LOGIN_GENERIC = 'Sign-in failed. Try again or use an email link.';
const DISMISSAL = 'You dismissed the passkey prompt';

/**
 * Install a `navigator.credentials` double for the signup ceremony.
 * Calls listed in `rejectNames` (1-based call index -> error name)
 * reject; every other call resolves with a fixture credential, the
 * same shape `signup-paywall.spec.ts` uses.
 */
async function installCreateDouble(
	page: Page,
	rejectNames: Record<number, string>
): Promise<void> {
	await page.addInitScript((rejects) => {
		class FixturePublicKeyCredential {
			id = 'fixture-credential';
			rawId = new Uint8Array([1]).buffer;
			type = 'public-key';
			authenticatorAttachment = 'platform';
			response = {
				clientDataJSON: new Uint8Array([2]).buffer,
				attestationObject: new Uint8Array([3]).buffer
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
				create: async () => {
					calls += 1;
					const name = (rejects as Record<number, string>)[calls];
					if (name) throw new DOMException('fixture ceremony rejection', name);
					return new FixturePublicKeyCredential();
				}
			}
		});
	}, rejectNames);
}

/** Same double for the login ceremony (`credentials.get`). */
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

interface SignupTraffic {
	startPosts: number;
	finishPosts: number;
	finishBodies: Array<Record<string, unknown>>;
}

async function routeSignup(page: Page, finishStatus = 200): Promise<SignupTraffic> {
	const traffic: SignupTraffic = { startPosts: 0, finishPosts: 0, finishBodies: [] };
	await page.route('**/v1/auth/passkey/register/start', async (route) => {
		traffic.startPosts += 1;
		await route.fulfill({
			status: 200,
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify({ challenge: 'Y2hhbGxlbmdl', user_id: 'dXNlcg', rp_id: 'localhost' })
		});
	});
	await page.route('**/v1/auth/passkey/register/finish', async (route) => {
		traffic.finishPosts += 1;
		traffic.finishBodies.push(route.request().postDataJSON() as Record<string, unknown>);
		await route.fulfill({
			status: finishStatus,
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify(
				finishStatus === 200 ? { ok: true } : { error: 'check your email and try again' }
			)
		});
	});
	return traffic;
}

async function reachSignupCodeStep(page: Page): Promise<void> {
	await page.goto('/signup');
	await page.getByTestId('signup-email').fill('alice@example.com');
	await page.getByTestId('signup-submit').click();
	await page.getByLabel('Email code').fill('fixture-code');
}

interface LoginTraffic {
	startPosts: number;
	finishPosts: number;
}

async function routeLogin(
	page: Page,
	{ startStatus = 200, finishStatus = 200 } = {}
): Promise<LoginTraffic> {
	const traffic: LoginTraffic = { startPosts: 0, finishPosts: 0 };
	await page.route('**/v1/auth/passkey/login/start', async (route) => {
		traffic.startPosts += 1;
		await route.fulfill({
			status: startStatus,
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify(
				startStatus === 200
					? { challenge: 'Y2hhbGxlbmdl', allow_credentials: [], rp_id: 'localhost' }
					: { error: 'check your email and try again' }
			)
		});
	});
	await page.route('**/v1/auth/passkey/login/finish', async (route) => {
		traffic.finishPosts += 1;
		await route.fulfill({
			status: finishStatus,
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify(
				finishStatus === 200 ? { ok: true } : { error: 'check your email and try again' }
			)
		});
	});
	return traffic;
}

async function startLogin(page: Page): Promise<void> {
	await page.goto('/login');
	await page.locator('input[type="email"]').fill('alice@example.com');
	await page.getByRole('button', { name: /sign in with passkey/i }).click();
}

for (const errorName of ['NotAllowedError', 'AbortError']) {
	test(`signup: dismissing the passkey prompt (${errorName}) says the code is still valid, without contacting finish`, async ({
		page
	}) => {
		await installCreateDouble(page, { 1: errorName });
		const traffic = await routeSignup(page);

		await reachSignupCodeStep(page);
		await page.getByRole('button', { name: /finish creating account/i }).click();

		const alert = page.getByRole('alert');
		await expect(alert).toContainText(DISMISSAL);
		await expect(alert).toContainText(/code is still valid/i);
		await expect(alert).not.toContainText('re-enter the code');
		await expect(alert).not.toContainText('fresh code');
		// The code the user typed survives, and the ceremony can be retried
		// in place — no start-over, no new code.
		await expect(page.getByLabel('Email code')).toBeVisible();
		await expect(page.getByLabel('Email code')).toHaveValue('fixture-code');
		await expect(
			page.getByRole('button', { name: /finish creating account/i })
		).toBeEnabled();
		expect(page.url()).toContain('/signup');
		expect(traffic.startPosts).toBe(1);
		expect(traffic.finishPosts).toBe(0);
	});

	test(`login: dismissing the passkey prompt (${errorName}) is not reported as a sign-in failure`, async ({
		page
	}) => {
		await installGetDouble(page, { 1: errorName });
		const traffic = await routeLogin(page);

		await startLogin(page);

		const alert = page.getByRole('alert');
		await expect(alert).toContainText(DISMISSAL);
		await expect(alert).not.toHaveText(LOGIN_GENERIC);
		await expect(page.locator('input[type="email"]')).toHaveValue('alice@example.com');
		await expect(
			page.getByRole('button', { name: /sign in with passkey/i })
		).toBeEnabled();
		expect(page.url()).toContain('/login');
		expect(traffic.startPosts).toBe(1);
		expect(traffic.finishPosts).toBe(0);
	});
}

test('signup: retry after dismissal finishes with the same code and challenge', async ({
	page
}) => {
	await installCreateDouble(page, { 1: 'NotAllowedError' });
	const traffic = await routeSignup(page);

	await reachSignupCodeStep(page);
	await page.getByRole('button', { name: /finish creating account/i }).click();
	await expect(page.getByRole('alert')).toContainText(DISMISSAL);

	await page.getByRole('button', { name: /finish creating account/i }).click();
	await page.waitForURL('/onboarding/byop');

	expect(traffic.startPosts).toBe(1);
	expect(traffic.finishPosts).toBe(1);
	expect(traffic.finishBodies[0]).toMatchObject({
		challenge: 'Y2hhbGxlbmdl',
		magic_token: 'fixture-code'
	});
});

test('login: retry after dismissal signs in', async ({ page }) => {
	await installGetDouble(page, { 1: 'AbortError' });
	const traffic = await routeLogin(page);

	await startLogin(page);
	await expect(page.getByRole('alert')).toContainText(DISMISSAL);

	await page.getByRole('button', { name: /sign in with passkey/i }).click();
	await page.waitForURL('/onboarding/provider');

	expect(traffic.finishPosts).toBe(1);
});

test('signup: a genuine finish failure keeps the exact current code message', async ({
	page
}) => {
	await installCreateDouble(page, {});
	const traffic = await routeSignup(page, 401);

	await reachSignupCodeStep(page);
	await page.getByRole('button', { name: /finish creating account/i }).click();

	const alert = page.getByRole('alert');
	await expect(alert).toHaveText(SIGNUP_GENERIC);
	await expect(alert).not.toContainText(DISMISSAL);
	expect(traffic.finishPosts).toBe(1);
});

test('signup: a non-dismissal ceremony error keeps the exact current code message', async ({
	page
}) => {
	await installCreateDouble(page, { 1: 'InvalidStateError' });
	const traffic = await routeSignup(page);

	await reachSignupCodeStep(page);
	await page.getByRole('button', { name: /finish creating account/i }).click();

	const alert = page.getByRole('alert');
	await expect(alert).toHaveText(SIGNUP_GENERIC);
	await expect(alert).not.toContainText(DISMISSAL);
	expect(traffic.finishPosts).toBe(0);
});

test('login: a genuine finish failure keeps the exact current sign-in message', async ({
	page
}) => {
	await installGetDouble(page, {});
	const traffic = await routeLogin(page, { finishStatus: 401 });

	await startLogin(page);

	const alert = page.getByRole('alert');
	await expect(alert).toHaveText(LOGIN_GENERIC);
	await expect(alert).not.toContainText(DISMISSAL);
	expect(traffic.finishPosts).toBe(1);
});

test('login: a refused start keeps the exact same message as a refused finish (non-enumerating)', async ({
	page
}) => {
	await installGetDouble(page, {});
	const traffic = await routeLogin(page, { startStatus: 401 });

	await startLogin(page);

	const alert = page.getByRole('alert');
	await expect(alert).toHaveText(LOGIN_GENERIC);
	await expect(alert).not.toContainText(DISMISSAL);
	expect(traffic.finishPosts).toBe(0);
});

test('login: a non-dismissal ceremony error keeps the exact current sign-in message', async ({
	page
}) => {
	await installGetDouble(page, { 1: 'SecurityError' });
	const traffic = await routeLogin(page);

	await startLogin(page);

	const alert = page.getByRole('alert');
	await expect(alert).toHaveText(LOGIN_GENERIC);
	await expect(alert).not.toContainText(DISMISSAL);
	expect(traffic.finishPosts).toBe(0);
});
