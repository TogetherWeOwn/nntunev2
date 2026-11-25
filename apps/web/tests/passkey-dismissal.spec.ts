import { expect, test, type BrowserContext, type Page } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';

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
	await expect(page).toHaveURL(/\/account\/billing$/);

	expect(traffic.startPosts).toBe(1);
	expect(traffic.finishPosts).toBe(1);
	expect(traffic.finishBodies[0]).toMatchObject({
		challenge: 'Y2hhbGxlbmdl',
		magic_token: 'fixture-code'
	});
	expect(traffic.finishBodies[0]).not.toHaveProperty('invite_code');
});

test('login: retry after dismissal signs in', async ({ page }) => {
	await installGetDouble(page, { 1: 'AbortError' });
	const traffic = await routeLogin(page);

	await startLogin(page);
	await expect(page.getByRole('alert')).toContainText(DISMISSAL);

	await page.getByRole('button', { name: /sign in with passkey/i }).click();
	// Sign-in navigates to /library, so the settled URL — like every
	// other sign-in spec — is /library.
	await page.waitForURL('/library');

	expect(traffic.finishPosts).toBe(1);
});

function privateSignupFixture() {
	const { signup, library } = PRIVATE_MUSIC;
	const code = 'synthetic_invite_codeA';
	const email = 'friend@example.test';
	const binding = 'A'.repeat(43);
	const cookieName = 'fixture_private_session';
	const session = `${cookieName}=synthetic`;
	const registerStart = '/v1/auth/passkey/register/start';
	const sessionCheck = '/v1/me/likes/session';
	const refused = { status: 401, json: { error: 'check your email and try again' } };
	const hasSession = (cookie: string | null) => cookie?.split('; ').includes(session);
	let redeemed = false;
	let revoked = false;
	let finishPosts = 0;

	async function install(page: Page) {
		await installCreateDouble(page, {});
		await page.route(`**${registerStart}`, (route) => {
			const input = route.request().postDataJSON();
			return !redeemed && input.email === email && input.invite_code === code
				? route.fulfill({ json: { challenge: 'Y2hhbGxlbmdl', user_id: 'dXNlcg', rp_id: 'localhost' } })
				: route.fulfill(refused);
		});
		await page.route('**/v1/auth/passkey/register/finish', (route) => {
			finishPosts += 1;
			if (redeemed) return route.fulfill(refused);
			expect(route.request().postDataJSON()).toMatchObject({ invite_code: code, magic_token: 'fixture-code' });
			redeemed = true;
			return route.fulfill({
				headers: { 'set-cookie': `${session}; Path=/; HttpOnly; SameSite=Lax` },
				json: { ok: true, private_listening: true }
			});
		});
		await page.route(`**${sessionCheck}`, async (route) => {
			if (!hasSession(await route.request().headerValue('cookie'))) return route.fulfill({ status: 401 });
			return route.fulfill({ json: { session_binding: binding } });
		});
		await page.route(`**${library.api}`, async (route) => {
			if (!hasSession(await route.request().headerValue('cookie'))) return route.fulfill({ status: 401 });
			if (revoked) return route.fulfill({ status: 403, json: { code: 'forbidden' } });
			return route.fulfill({ headers: { 'x-nntune-session-binding': binding }, json: [{ id: 'fixture-track', title: 'Synthetic track' }] });
		});
	}

	async function openInvite(page: Page) {
		await page.goto(`${signup.path}#invite=${code}`);
		await expect(page.getByLabel(/invite code/i)).toHaveValue(code);
		await expect(page).toHaveURL(signup.path);
		await page.getByTestId(signup.email).fill(email);
	}

	const track = (page: Page) => page.getByTestId(library.item).filter({ hasText: 'Synthetic track' });
	async function redeem(page: Page) {
		await openInvite(page);
		await page.getByTestId(signup.submit).click();
		await page.getByLabel(signup.emailCode, { exact: true }).fill('fixture-code');
		await page.getByRole('button', { name: signup.finish, exact: true }).click();
		await expect(page).toHaveURL(library.path);
		await expect(track(page)).toBeVisible();
	}

	return {
		install, openInvite, redeem, track, code, email, cookieName, registerStart, sessionCheck,
		revoke: () => { revoked = true; },
		get finishPosts() { return finishPosts; }
	};
}

test('private listening invite opens Library and refusal clears it', async ({ page }) => {
	const { signup, library } = PRIVATE_MUSIC;
	const fixture = privateSignupFixture();
	await fixture.install(page);
	await page.goto(signup.path);
	await page.getByTestId(signup.email).fill(fixture.email);
	await page.getByTestId(signup.submit).click();
	await expect(page.getByTestId(signup.error)).toBeVisible();
	await expect(page.getByLabel(signup.emailCode, { exact: true })).toHaveCount(0);
	await fixture.redeem(page);
	fixture.revoke();
	await page.getByRole('button', { name: library.refresh, exact: true }).click();
	await expect(page.getByText(library.forbidden, { exact: true })).toBeVisible();
	await expect(fixture.track(page)).toHaveCount(0);
});

for (const phase of ['start', 'finish'] as const) {
	test(`private invite stays private when registration ${phase} redirects`, async ({ page, baseURL }) => {
		const origin = new URL(baseURL ?? 'about:blank');
		if (!['localhost', '127.0.0.1', '[::1]'].includes(origin.hostname)) {
			throw new Error('Private invite signup requires a loopback browser fixture');
		}
		const { signup } = PRIVATE_MUSIC;
		const fixture = privateSignupFixture();
		const endpoint = `/v1/auth/passkey/register/${phase}`;
		const redirectStatus = phase === 'start' ? 307 : 308;
		const submissions: { method: string; body: unknown }[] = [];
		let leakedUrl = false;
		let leakedDiagnostic = false;
		page.on('request', (request) => { leakedUrl ||= request.url().includes(fixture.code); });
		page.on('console', (message) => { leakedDiagnostic ||= message.text().includes(fixture.code); });
		page.on('pageerror', (error) => { leakedDiagnostic ||= error.message.includes(fixture.code); });
		await fixture.install(page);
		await page.route(new URL(endpoint, origin).href, (route) => {
			const request = route.request();
			submissions.push({ method: request.method(), body: request.postDataJSON() });
			return route.fulfill({
				status: redirectStatus,
				headers: { location: `/unexpected-signup-target?fixture=${fixture.code}` }
			});
		});

		await fixture.openInvite(page);
		const start = page.getByTestId(signup.submit);
		const submit = phase === 'start' ? start : page.getByRole('button', { name: signup.finish, exact: true });
		if (phase === 'finish') {
			await start.click();
			await page.getByLabel(signup.emailCode, { exact: true }).fill('fixture-code');
		}
		await submit.click();
		await expect(page.getByRole('alert')).toHaveText(
			phase === 'start' ? 'Something went wrong. Try again.' : SIGNUP_GENERIC
		);
		expect(submissions).toMatchObject([{ method: 'POST', body: { invite_code: fixture.code } }]);
		await expect(page).toHaveURL(signup.path);
		expect(leakedUrl, 'signup requested the credential-bearing redirect target').toBe(false);
		expect(leakedDiagnostic, 'a browser diagnostic included the synthetic invite').toBe(false);
	});
}

test('a spent private invite refuses signup in a second browser context', async ({ page, context, browser, baseURL }) => {
	const { signup, library } = PRIVATE_MUSIC;
	const origin = new URL(baseURL ?? 'about:blank');
	if (!['localhost', '127.0.0.1', '[::1]'].includes(origin.hostname)) {
		throw new Error('Private invite signup requires a loopback browser fixture');
	}
	const fixture = privateSignupFixture();
	const restrictToFixture = (context: BrowserContext) =>
		context.route('**/*', (route) => new URL(route.request().url()).origin === origin.origin ? route.continue() : route.abort());
	await restrictToFixture(context);
	await fixture.install(page);
	await fixture.redeem(page);

	const secondContext = await browser.newContext({ baseURL, serviceWorkers: 'block' });
	try {
		await restrictToFixture(secondContext);
		const secondPage = await secondContext.newPage();
		await fixture.install(secondPage);
		// Use the invited address again so the only refusal is the spent code.
		await fixture.openInvite(secondPage);
		const registerStartUrl = new URL(fixture.registerStart, origin).href;
		const refusal = secondPage.waitForResponse((response) =>
			response.url() === registerStartUrl && response.request().method() === 'POST');
		await secondPage.getByTestId(signup.submit).click();
		const response = await refusal;
		expect(response.request().postDataJSON()).toEqual({ email: fixture.email, invite_code: fixture.code });
		expect(response.status()).toBe(401);
		await expect(secondPage.getByTestId(signup.error)).toHaveText('Something went wrong. Try again.');
		await expect(secondPage.getByLabel(signup.emailCode, { exact: true })).toHaveCount(0);
		await expect(secondPage).toHaveURL(signup.path);
		expect((await secondContext.cookies()).map(({ name }) => name)).not.toContain(fixture.cookieName);
		expect(await secondPage.evaluate(async (path) => (await fetch(path)).status, fixture.sessionCheck)).toBe(401);

		const libraryUrl = new URL(library.api, origin).href;
		const [deniedLibrary] = await Promise.all([
			secondPage.waitForResponse(libraryUrl),
			secondPage.goto(library.path)
		]);
		expect(deniedLibrary.status()).toBe(401);
		await expect(secondPage.getByText('You are signed out or do not have access to this library.', { exact: true })).toBeVisible();
		await expect(secondPage.getByTestId(library.item)).toHaveCount(0);
		expect(fixture.finishPosts).toBe(1);

		const [allowedLibrary] = await Promise.all([
			page.waitForResponse(libraryUrl),
			page.reload()
		]);
		expect(allowedLibrary.status()).toBe(200);
		await expect(page).toHaveURL(library.path);
		await expect(fixture.track(page)).toBeVisible();
	} finally {
		await secondContext.close();
	}
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
