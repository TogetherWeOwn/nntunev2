import { expect, test, type Page } from '@playwright/test';

/**
 * Acceptance: an in-flight passkey sign-in must not be flagged as
 * failed, and neither must a click with no email.
 *
 * Pre-fix, `signInWithPasskey` opened with one combined guard —
 * `passkeyStatus === 'working' || email.trim().length === 0` — that set
 * the generic "Sign-in failed" state. The button stayed enabled while
 * a ceremony was pending, so a second click (or a click before typing
 * an email) told the user their sign-in had failed while the first
 * ceremony was still running / before anything was attempted.
 *
 * Required behavior:
 * - while a ceremony is in flight the button is disabled, a working
 *   status is shown, and a re-click attempt is a no-op (one start,
 *   one ceremony, no failure alert; the in-flight ceremony can still
 *   finish and sign in);
 * - an empty/whitespace email shows an inline prompt asking for the
 *   email, contacts no endpoint, and leaves the button usable;
 * - a genuine failure keeps the exact current generic message.
 *
 * Dismissal messaging is pinned separately in `passkey-dismissal.spec.ts`.
 */

const LOGIN_GENERIC = 'Sign-in failed. Try again or use an email link.';

declare global {
	interface Window {
		__passkeyGetCalls?: number;
		__releasePasskey?: () => void;
	}
}

/**
 * `navigator.credentials.get` double. With `hold`, the ceremony stays
 * pending until the test calls `window.__releasePasskey()`; otherwise
 * it resolves immediately with a fixture credential (same shape as
 * `passkey-dismissal.spec.ts`). Calls are counted on
 * `window.__passkeyGetCalls`.
 */
async function installGetDouble(page: Page, { hold = false } = {}): Promise<void> {
	await page.addInitScript((holdCeremony) => {
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
		window.__passkeyGetCalls = 0;
		Object.defineProperty(navigator, 'credentials', {
			value: {
				get: () => {
					window.__passkeyGetCalls = (window.__passkeyGetCalls ?? 0) + 1;
					if (!holdCeremony) return Promise.resolve(new FixturePublicKeyCredential());
					return new Promise((resolve) => {
						window.__releasePasskey = () => resolve(new FixturePublicKeyCredential());
					});
				}
			}
		});
	}, hold);
}

interface LoginTraffic {
	startPosts: number;
	finishPosts: number;
}

async function routeLogin(page: Page, { finishStatus = 200 } = {}): Promise<LoginTraffic> {
	const traffic: LoginTraffic = { startPosts: 0, finishPosts: 0 };
	await page.route('**/v1/auth/passkey/login/start', async (route) => {
		traffic.startPosts += 1;
		await route.fulfill({
			status: 200,
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify({ challenge: 'Y2hhbGxlbmdl', allow_credentials: [], rp_id: 'localhost' })
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

async function getCalls(page: Page): Promise<number> {
	return page.evaluate(() => window.__passkeyGetCalls ?? 0);
}

test('login: re-clicking while a passkey ceremony is in flight is a no-op, never a failure', async ({
	page
}) => {
	await installGetDouble(page, { hold: true });
	const traffic = await routeLogin(page);

	await page.goto('/login');
	await page.locator('input[type="email"]').fill('alice@example.com');
	const button = page.getByRole('button', { name: /sign in with passkey/i });
	await button.click();

	// The ceremony is now pending inside credentials.get.
	await expect.poll(() => traffic.startPosts).toBe(1);
	await expect.poll(() => getCalls(page)).toBe(1);

	// In flight: button disabled, working status shown, no failure.
	await expect(button).toBeDisabled();
	const status = page.getByRole('status');
	await expect(status).toBeVisible();
	await expect(status).toContainText(/sign|passkey|working|wait/i);
	await expect(page.getByRole('alert')).toHaveCount(0);
	await expect(page.getByText(LOGIN_GENERIC)).toHaveCount(0);

	// A re-click attempt (forced past the disabled button, and a
	// programmatic click) changes nothing.
	await button.click({ force: true }).catch(() => {});
	await button.evaluate((el) => (el as HTMLButtonElement).click()).catch(() => {});
	await expect(page.getByRole('alert')).toHaveCount(0);
	await expect(page.getByText(LOGIN_GENERIC)).toHaveCount(0);
	await expect(button).toBeDisabled();
	expect(traffic.startPosts).toBe(1);
	expect(await getCalls(page)).toBe(1);
	expect(traffic.finishPosts).toBe(0);

	// The original ceremony was not poisoned: releasing it signs in.
	await page.evaluate(() => window.__releasePasskey?.());
	await page.waitForURL('/onboarding/byop');
	expect(traffic.startPosts).toBe(1);
	expect(traffic.finishPosts).toBe(1);
});

for (const [label, email] of [
	['empty', ''],
	['whitespace-only', '   ']
] as const) {
	test(`login: ${label} email prompts for the email instead of failing the sign-in`, async ({
		page
	}) => {
		await installGetDouble(page);
		const traffic = await routeLogin(page);

		await page.goto('/login');
		if (email.length > 0) await page.locator('input[type="email"]').fill(email);
		const button = page.getByRole('button', { name: /sign in with passkey/i });
		await button.click();

		// Inline prompt naming the email, and never the generic failure.
		await expect(page.getByText(LOGIN_GENERIC)).toHaveCount(0);
		const prompt = page
			.getByRole('status')
			.or(page.getByRole('alert'))
			.filter({ hasText: /email/i });
		await expect(prompt).toBeVisible();
		await expect(prompt).not.toContainText('Sign-in failed');

		// Nothing was attempted, and the form is still usable.
		await expect(button).toBeEnabled();
		expect(page.url()).toContain('/login');
		expect(traffic.startPosts).toBe(0);
		expect(traffic.finishPosts).toBe(0);
		expect(await getCalls(page)).toBe(0);
	});
}

test('login: after the empty-email prompt, entering an email and retrying signs in', async ({
	page
}) => {
	await installGetDouble(page);
	const traffic = await routeLogin(page);

	await page.goto('/login');
	const button = page.getByRole('button', { name: /sign in with passkey/i });
	await button.click();
	await expect(page.getByText(LOGIN_GENERIC)).toHaveCount(0);
	await expect(
		page.getByRole('status').or(page.getByRole('alert')).filter({ hasText: /email/i })
	).toBeVisible();

	await page.locator('input[type="email"]').fill('alice@example.com');
	await button.click();
	await page.waitForURL('/onboarding/byop');

	expect(traffic.startPosts).toBe(1);
	expect(traffic.finishPosts).toBe(1);
});

test('login: a genuine finish failure still shows the exact generic message and re-enables the button', async ({
	page
}) => {
	await installGetDouble(page);
	const traffic = await routeLogin(page, { finishStatus: 401 });

	await page.goto('/login');
	await page.locator('input[type="email"]').fill('alice@example.com');
	const button = page.getByRole('button', { name: /sign in with passkey/i });
	await button.click();

	await expect(page.getByRole('alert')).toHaveText(LOGIN_GENERIC);
	await expect(button).toBeEnabled();
	expect(traffic.finishPosts).toBe(1);
});
