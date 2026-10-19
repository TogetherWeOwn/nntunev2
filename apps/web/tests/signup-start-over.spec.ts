import { expect, test, type Page } from '@playwright/test';

/**
 * Acceptance: a signup user with an unusable emailed code (typically a
 * mistyped email) can start over BEFORE submitting it — from the
 * code-sent, dismissed, and code-error states — instead of inducing a
 * finish error or reloading.
 *
 * Required behavior:
 * - the existing Start over action is offered in code-sent, dismissed,
 *   and code-error; it is never active while finishing or after the
 *   account is created;
 * - starting over reuses the reset semantics: pending challenge,
 *   magic code, and invite code are cleared and the user returns to an
 *   editable email — and the reset itself sends no request (no
 *   registerFinish, no new registerStart, no authenticator ceremony);
 * - the next explicit start obtains a new challenge and the next finish
 *   uses that fresh challenge, never the discarded one;
 * - the dismissal in-place retry (same code, same challenge) and the
 *   generic non-enumerating code-error copy are preserved.
 *
 * The finishing/created exclusion is pinned behaviorally for finishing
 * (held ceremony) and against the route source for `created` (the
 * success state navigates away immediately, so it cannot be held) in
 * `src/routes/signup/page.test.ts`.
 */

const SIGNUP_GENERIC =
	"That didn't work — re-enter the code (and invite code, if you have one) from the email and try again. If it keeps failing, start over to get a fresh code.";
const DISMISSAL = 'You dismissed the passkey prompt';
// Valid base64 (the client decodes the challenge into creation options):
// 'first' then 'second'.
const FIRST_CHALLENGE = 'Zmlyc3Q=';
const SECOND_CHALLENGE = 'c2Vjb25k';

declare global {
	interface Window {
		__passkeyCreateCalls?: number;
		__releasePasskeyCreate?: () => void;
	}
}

/**
 * `navigator.credentials.create` double (same fixture shape as
 * `passkey-dismissal.spec.ts`). Calls listed in `rejectNames` (1-based
 * call index -> error name) reject; with `hold` the first call stays
 * pending until the test calls `window.__releasePasskeyCreate()`;
 * every other call resolves. Calls are counted on
 * `window.__passkeyCreateCalls`.
 */
async function installCreateDouble(
	page: Page,
	{ rejectNames = {}, hold = false }: { rejectNames?: Record<number, string>; hold?: boolean } = {}
): Promise<void> {
	await page.addInitScript(
		({ rejects, holdCeremony }) => {
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
			window.__passkeyCreateCalls = 0;
			Object.defineProperty(navigator, 'credentials', {
				value: {
					create: async () => {
						window.__passkeyCreateCalls = (window.__passkeyCreateCalls ?? 0) + 1;
						const calls = window.__passkeyCreateCalls;
						if (holdCeremony && calls === 1) {
							await new Promise<void>((resolve) => {
								window.__releasePasskeyCreate = () => resolve();
							});
						}
						const name = (rejects as Record<number, string>)[calls];
						if (name) throw new DOMException('fixture ceremony rejection', name);
						return new FixturePublicKeyCredential();
					}
				}
			});
		},
		{ rejects: rejectNames, holdCeremony: hold }
	);
}

interface SignupTraffic {
	startPosts: number;
	startBodies: Array<Record<string, unknown>>;
	finishPosts: number;
	finishBodies: Array<Record<string, unknown>>;
}

/** Start answers a fresh challenge per call (first, then second, ...). */
async function routeSignup(page: Page, { finishStatus = 200 } = {}): Promise<SignupTraffic> {
	const traffic: SignupTraffic = { startPosts: 0, startBodies: [], finishPosts: 0, finishBodies: [] };
	const challenges = [FIRST_CHALLENGE, SECOND_CHALLENGE];
	await page.route('**/v1/auth/passkey/register/start', async (route) => {
		traffic.startPosts += 1;
		traffic.startBodies.push(route.request().postDataJSON() as Record<string, unknown>);
		const challenge = challenges[Math.min(traffic.startPosts - 1, challenges.length - 1)];
		await route.fulfill({
			status: 200,
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify({ challenge, user_id: 'dXNlcg', rp_id: 'localhost' })
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

async function createCalls(page: Page): Promise<number> {
	return page.evaluate(() => window.__passkeyCreateCalls ?? 0);
}

function startOver(page: Page) {
	return page.getByRole('button', { name: /start over/i });
}

async function reachSignupCodeStep(page: Page, email = 'alice@example.com'): Promise<void> {
	await page.goto('/signup');
	await page.getByTestId('signup-email').fill(email);
	await page.getByTestId('signup-submit').click();
	await expect(page.getByLabel('Email code')).toBeVisible();
}

test('signup: Start over from code-sent sends nothing; the next start finishes with the fresh challenge', async ({
	page
}) => {
	await installCreateDouble(page);
	const traffic = await routeSignup(page);

	await reachSignupCodeStep(page, 'alice@example.com');
	// Code-sent offers the reset before any code is submitted.
	await expect(startOver(page)).toBeVisible();
	await page.getByLabel('Email code').fill('stale-code');
	await page.getByLabel(/invite code/i).fill('stale-invite');

	await startOver(page).click();

	// The reset alone contacts nothing and requests no authenticator.
	expect(traffic.startPosts).toBe(1);
	expect(traffic.finishPosts).toBe(0);
	expect(await createCalls(page)).toBe(0);
	// Back at an editable email with the mistyped address preserved for
	// fixing; the code step (and any alert) is gone.
	const email = page.getByTestId('signup-email');
	await expect(email).toBeVisible();
	await expect(email).toBeEnabled();
	await expect(email).toHaveValue('alice@example.com');
	await expect(page.getByLabel('Email code')).toHaveCount(0);
	await expect(page.getByRole('alert')).toHaveCount(0);

	// A subsequent explicit start obtains a new challenge; both typed
	// codes were cleared by the reset.
	await email.fill('alice.corrected@example.com');
	await page.getByTestId('signup-submit').click();
	const code = page.getByLabel('Email code');
	await expect(code).toBeVisible();
	await expect(code).toHaveValue('');
	await expect(page.getByLabel(/invite code/i)).toHaveValue('');
	expect(traffic.startPosts).toBe(2);
	expect(traffic.startBodies[1]).toMatchObject({ email: 'alice.corrected@example.com' });

	await code.fill('fresh-code');
	await page.getByRole('button', { name: /finish creating account/i }).click();
	await page.waitForURL('/onboarding/byop');

	// The finish used the fresh challenge and code — never the discarded ones.
	expect(traffic.finishPosts).toBe(1);
	expect(traffic.finishBodies[0]).toMatchObject({
		challenge: SECOND_CHALLENGE,
		magic_token: 'fresh-code'
	});
	expect(traffic.finishBodies[0]?.challenge).not.toBe(FIRST_CHALLENGE);
	expect(traffic.finishBodies[0]).not.toHaveProperty('invite_code');
});

test('signup: dismissed state offers Start over alongside the in-place retry', async ({
	page
}) => {
	await installCreateDouble(page, { rejectNames: { 1: 'NotAllowedError' } });
	const traffic = await routeSignup(page);

	await reachSignupCodeStep(page);
	await page.getByLabel('Email code').fill('fixture-code');
	await page.getByRole('button', { name: /finish creating account/i }).click();
	await expect(page.getByRole('alert')).toContainText(DISMISSAL);

	// Both exits exist: retry in place with the same challenge, or start over.
	await expect(startOver(page)).toBeVisible();
	await expect(page.getByRole('button', { name: /finish creating account/i })).toBeEnabled();
	expect(traffic.finishPosts).toBe(0);

	await startOver(page).click();

	await expect(page.getByTestId('signup-email')).toBeVisible();
	await expect(page.getByRole('alert')).toHaveCount(0);
	expect(traffic.startPosts).toBe(1);
	expect(traffic.finishPosts).toBe(0);
	expect(await createCalls(page)).toBe(1);
});

test('signup: code-error state offers Start over with the unchanged generic copy', async ({
	page
}) => {
	await installCreateDouble(page);
	const traffic = await routeSignup(page, { finishStatus: 401 });

	await reachSignupCodeStep(page);
	await page.getByLabel('Email code').fill('wrong-code');
	await page.getByRole('button', { name: /finish creating account/i }).click();

	// Non-enumerating generic copy, exactly as before.
	await expect(page.getByRole('alert')).toHaveText(SIGNUP_GENERIC);
	await expect(startOver(page)).toBeVisible();
	expect(traffic.finishPosts).toBe(1);

	await startOver(page).click();

	await expect(page.getByTestId('signup-email')).toBeVisible();
	await expect(page.getByRole('alert')).toHaveCount(0);
	expect(traffic.startPosts).toBe(1);
	expect(traffic.finishPosts).toBe(1);
});

test('signup: Start over is hidden while finishing and returns with the settled state', async ({
	page
}) => {
	await installCreateDouble(page, { hold: true });
	const traffic = await routeSignup(page, { finishStatus: 401 });

	await reachSignupCodeStep(page);
	await page.getByLabel('Email code').fill('fixture-code');
	const finish = page.getByRole('button', { name: /finish creating account/i });
	await finish.click();

	// Finishing: the ceremony is pending, the reset is not active.
	await expect.poll(() => createCalls(page)).toBe(1);
	await expect(page.getByRole('button', { name: /creating/i })).toBeVisible();
	await expect(startOver(page)).toHaveCount(0);

	// Once the finish settles (here: code-error), the reset is back.
	await page.evaluate(() => window.__releasePasskeyCreate?.());
	await expect(page.getByRole('alert')).toHaveText(SIGNUP_GENERIC);
	await expect(startOver(page)).toBeVisible();
	expect(traffic.finishPosts).toBe(1);
});

test('signup: Start over activates from the keyboard', async ({ page }) => {
	await installCreateDouble(page);
	const traffic = await routeSignup(page);

	await reachSignupCodeStep(page);
	await page.getByLabel('Email code').fill('stale-code');

	await startOver(page).focus();
	await page.keyboard.press('Enter');

	await expect(page.getByTestId('signup-email')).toBeVisible();
	await expect(page.getByLabel('Email code')).toHaveCount(0);
	expect(traffic.startPosts).toBe(1);
	expect(traffic.finishPosts).toBe(0);
	expect(await createCalls(page)).toBe(0);
});
