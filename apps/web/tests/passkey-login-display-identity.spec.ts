import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Acceptance: the passkey login display identity stays bound to the
 * email the attempt was started with.
 *
 * Pre-fix, the login page read the mutable email field twice: once to
 * start the ceremony and again — after the awaited ceremony/finish —
 * to stash the signed-in display label. The email input stayed
 * editable while a passkey ceremony was pending, so editing it
 * mid-flight relabeled the completed sign-in with an address that was
 * never attempted. That label is untrusted display data, never account
 * authority (the server decides identity from the credential), but it
 * must not be silently rewritable by a mid-flight edit.
 *
 * Required behavior:
 * - the normalized (trimmed) attempted email is captured once, before
 *   the ceremony starts; that same snapshot is sent to login/start and
 *   stored as the post-success display label — mutable form state is
 *   not re-read after an await, so even a programmatic mid-flight
 *   mutation of the input cannot change the stored label;
 * - the email input cannot be edited while a passkey attempt is in
 *   flight (disabled or read-only); dismissal and genuine errors
 *   restore editing with the attempted email still in place;
 * - duplicate-click suppression, empty-email handling, the exact
 *   generic non-enumerating failure message, and the post-success
 *   destination are preserved (also pinned in
 *   `passkey-in-flight.spec.ts` and `passkey-dismissal.spec.ts`).
 *
 * All auth endpoints and `navigator.credentials` are mocked; no real
 * passkey, provider, or session is involved.
 */

const LOGIN_GENERIC = 'Sign-in failed. Try again or use an email link.';
const DISMISSAL = 'You dismissed the passkey prompt';
const LABEL_KEY = 'nntune:signed-in-email';
const ATTEMPTED = 'alice@example.com';
const RELABEL = 'mallory@example.com';

declare global {
	interface Window {
		__passkeyGetCalls?: number;
		__releasePasskey?: () => void;
		__dismissPasskey?: (errorName?: string) => void;
	}
}

/**
 * `navigator.credentials.get` double. With `hold`, the ceremony stays
 * pending until the test calls `window.__releasePasskey()` (resolve
 * with a fixture credential) or `window.__dismissPasskey(name?)`
 * (reject with a `DOMException`, `NotAllowedError` by default);
 * otherwise it resolves immediately. Calls are counted on
 * `window.__passkeyGetCalls`. Same fixture shape as
 * `passkey-in-flight.spec.ts`.
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
					return new Promise((resolve, reject) => {
						window.__releasePasskey = () => resolve(new FixturePublicKeyCredential());
						window.__dismissPasskey = (errorName = 'NotAllowedError') =>
							reject(new DOMException('fixture ceremony rejection', errorName));
					});
				}
			}
		});
	}, hold);
}

interface LoginTraffic {
	startBodies: Array<Record<string, unknown>>;
	finishPosts: number;
}

async function routeLogin(page: Page, { finishStatus = 200 } = {}): Promise<LoginTraffic> {
	const traffic: LoginTraffic = { startBodies: [], finishPosts: 0 };
	await page.route('**/v1/auth/passkey/login/start', async (route) => {
		traffic.startBodies.push(route.request().postDataJSON() as Record<string, unknown>);
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

async function storedLabel(page: Page): Promise<string | null> {
	return page.evaluate((key) => window.sessionStorage.getItem(key), LABEL_KEY);
}

async function isLocked(input: Locator): Promise<boolean> {
	return input.evaluate((el) => {
		const field = el as HTMLInputElement;
		return field.disabled || field.readOnly;
	});
}

/** Start a held passkey attempt and wait until the ceremony is pending. */
async function startHeldAttempt(page: Page, traffic: LoginTraffic, email: string): Promise<Locator> {
	await page.goto('/login');
	const input = page.locator('input[type="email"]');
	await input.fill(email);
	await page.getByRole('button', { name: /sign in with passkey/i }).click();
	await expect.poll(() => traffic.startBodies.length).toBe(1);
	await expect.poll(() => getCalls(page)).toBe(1);
	return input;
}

test('login: email cannot be edited while a passkey ceremony is in flight, and the stored display label is the attempted email', async ({
	page
}) => {
	await installGetDouble(page, { hold: true });
	const traffic = await routeLogin(page);
	const input = await startHeldAttempt(page, traffic, ATTEMPTED);

	// In flight: the email field is not editable and still shows the
	// attempted address. A user-level edit attempt cannot land.
	await expect.poll(() => isLocked(input)).toBe(true);
	await input.fill(RELABEL, { timeout: 1_000 }).catch(() => {});
	await expect(input).toHaveValue(ATTEMPTED);
	expect(traffic.startBodies).toEqual([{ email: ATTEMPTED }]);

	// Releasing the ceremony signs in and labels the sign-in with the
	// attempted email — the only address ever sent to login/start.
	await page.evaluate(() => window.__releasePasskey?.());
	await page.waitForURL('/onboarding/byop');
	expect(traffic.startBodies).toEqual([{ email: ATTEMPTED }]);
	expect(traffic.finishPosts).toBe(1);
	await expect.poll(() => storedLabel(page)).toBe(ATTEMPTED);
});

test('login: a programmatic mid-flight mutation of the email field cannot relabel the completed sign-in', async ({
	page
}) => {
	await installGetDouble(page, { hold: true });
	const traffic = await routeLogin(page);
	const input = await startHeldAttempt(page, traffic, ATTEMPTED);
	await expect.poll(() => isLocked(input)).toBe(true);

	// Bypass the edit lock the way script (or a forced DOM write) can:
	// set the value through the native setter and fire the events a
	// real edit fires. Disabling the field alone does not stop this —
	// only using the snapshot captured at attempt start does.
	await input.evaluate((el, relabel) => {
		const field = el as HTMLInputElement;
		const setter = Object.getOwnPropertyDescriptor(
			window.HTMLInputElement.prototype,
			'value'
		)?.set;
		if (setter) setter.call(field, relabel);
		else field.value = relabel;
		field.dispatchEvent(new Event('input', { bubbles: true }));
		field.dispatchEvent(new Event('change', { bubbles: true }));
	}, RELABEL);
	await expect(input).toHaveValue(RELABEL);

	await page.evaluate(() => window.__releasePasskey?.());
	await page.waitForURL('/onboarding/byop');

	// The request and the display label both stay with the attempted
	// email; the mutated field value was never adopted.
	expect(traffic.startBodies).toEqual([{ email: ATTEMPTED }]);
	expect(traffic.finishPosts).toBe(1);
	await expect.poll(() => storedLabel(page)).toBe(ATTEMPTED);
});

test('login: the trimmed attempted email is the snapshot sent to login/start and stored as the display label', async ({
	page
}) => {
	await installGetDouble(page, { hold: true });
	const traffic = await routeLogin(page);
	await startHeldAttempt(page, traffic, `  ${ATTEMPTED}  `);

	expect(traffic.startBodies).toEqual([{ email: ATTEMPTED }]);

	await page.evaluate(() => window.__releasePasskey?.());
	await page.waitForURL('/onboarding/byop');
	expect(traffic.finishPosts).toBe(1);
	await expect.poll(() => storedLabel(page)).toBe(ATTEMPTED);
});

test('login: dismissing the held passkey prompt restores email editing and stores no display label', async ({
	page
}) => {
	await installGetDouble(page, { hold: true });
	const traffic = await routeLogin(page);
	const input = await startHeldAttempt(page, traffic, ATTEMPTED);
	await expect.poll(() => isLocked(input)).toBe(true);

	await page.evaluate(() => window.__dismissPasskey?.('NotAllowedError'));

	// Dismissal messaging is preserved, editing is restored in fact
	// (not just by attribute), and nothing was signed in or labeled.
	const alert = page.getByRole('alert');
	await expect(alert).toContainText(DISMISSAL);
	await expect(alert).not.toHaveText(LOGIN_GENERIC);
	await expect.poll(() => isLocked(input)).toBe(false);
	await expect(input).toHaveValue(ATTEMPTED);
	await input.fill(RELABEL);
	await expect(input).toHaveValue(RELABEL);
	await expect(page.getByRole('button', { name: /sign in with passkey/i })).toBeEnabled();
	expect(page.url()).toContain('/login');
	expect(traffic.startBodies).toEqual([{ email: ATTEMPTED }]);
	expect(traffic.finishPosts).toBe(0);
	expect(await storedLabel(page)).toBeNull();
});

test('login: a genuine finish failure after a held ceremony restores email editing, keeps the generic message, and stores no display label', async ({
	page
}) => {
	await installGetDouble(page, { hold: true });
	const traffic = await routeLogin(page, { finishStatus: 401 });
	const input = await startHeldAttempt(page, traffic, ATTEMPTED);
	await expect.poll(() => isLocked(input)).toBe(true);

	await page.evaluate(() => window.__releasePasskey?.());

	await expect(page.getByRole('alert')).toHaveText(LOGIN_GENERIC);
	await expect.poll(() => isLocked(input)).toBe(false);
	await expect(input).toHaveValue(ATTEMPTED);
	await input.fill(RELABEL);
	await expect(input).toHaveValue(RELABEL);
	await expect(page.getByRole('button', { name: /sign in with passkey/i })).toBeEnabled();
	expect(page.url()).toContain('/login');
	expect(traffic.startBodies).toEqual([{ email: ATTEMPTED }]);
	expect(traffic.finishPosts).toBe(1);
	expect(await storedLabel(page)).toBeNull();
});

test('login: a non-dismissal ceremony error restores email editing and stores no display label', async ({
	page
}) => {
	await installGetDouble(page, { hold: true });
	const traffic = await routeLogin(page);
	const input = await startHeldAttempt(page, traffic, ATTEMPTED);
	await expect.poll(() => isLocked(input)).toBe(true);

	await page.evaluate(() => window.__dismissPasskey?.('SecurityError'));

	await expect(page.getByRole('alert')).toHaveText(LOGIN_GENERIC);
	await expect.poll(() => isLocked(input)).toBe(false);
	await expect(input).toHaveValue(ATTEMPTED);
	await expect(page.getByRole('button', { name: /sign in with passkey/i })).toBeEnabled();
	expect(page.url()).toContain('/login');
	expect(traffic.finishPosts).toBe(0);
	expect(await storedLabel(page)).toBeNull();
});

test('login: duplicate clicks during a held ceremony stay a no-op and the completed sign-in is labeled with the attempted email', async ({
	page
}) => {
	await installGetDouble(page, { hold: true });
	const traffic = await routeLogin(page);
	await startHeldAttempt(page, traffic, ATTEMPTED);

	const button = page.getByRole('button', { name: /sign in with passkey/i });
	await expect(button).toBeDisabled();
	await button.click({ force: true }).catch(() => {});
	await button.evaluate((el) => (el as HTMLButtonElement).click()).catch(() => {});
	await expect(page.getByRole('alert')).toHaveCount(0);
	await expect(page.getByText(LOGIN_GENERIC)).toHaveCount(0);
	expect(traffic.startBodies).toEqual([{ email: ATTEMPTED }]);
	expect(await getCalls(page)).toBe(1);
	expect(traffic.finishPosts).toBe(0);

	await page.evaluate(() => window.__releasePasskey?.());
	await page.waitForURL('/onboarding/byop');
	expect(traffic.startBodies).toEqual([{ email: ATTEMPTED }]);
	expect(traffic.finishPosts).toBe(1);
	await expect.poll(() => storedLabel(page)).toBe(ATTEMPTED);
});

for (const [label, email] of [
	['empty', ''],
	['whitespace-only', '   ']
] as const) {
	test(`login: ${label} email attempts nothing, stores no display label, and leaves the email editable`, async ({
		page
	}) => {
		await installGetDouble(page);
		const traffic = await routeLogin(page);

		await page.goto('/login');
		const input = page.locator('input[type="email"]');
		if (email.length > 0) await input.fill(email);
		const button = page.getByRole('button', { name: /sign in with passkey/i });
		await button.click();

		await expect(page.getByText(LOGIN_GENERIC)).toHaveCount(0);
		await expect(
			page.getByRole('status').or(page.getByRole('alert')).filter({ hasText: /email/i })
		).toBeVisible();
		await expect.poll(() => isLocked(input)).toBe(false);
		await expect(button).toBeEnabled();
		expect(page.url()).toContain('/login');
		expect(traffic.startBodies).toEqual([]);
		expect(traffic.finishPosts).toBe(0);
		expect(await getCalls(page)).toBe(0);
		expect(await storedLabel(page)).toBeNull();
	});
}
