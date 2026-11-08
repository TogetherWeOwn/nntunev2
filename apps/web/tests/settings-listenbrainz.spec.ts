import AxeBuilder from '@axe-core/playwright';
import { expect, test, type Page, type Route } from '@playwright/test';

/**
 * Pre-PUT Settings > ListenBrainz section: token-free status and Unlink
 * only. Every API response is mocked (no account, provider or database).
 * Pins the browser-only behaviour the node tests cannot: the dialogs, focus
 * moves, the offline guard at send time, the single post-passkey DELETE
 * retry, and Cancel/Escape never retrying.
 *
 * Pre-PUT gaps documented for UX (not designed here): there is no username
 * field, so focus after unlink goes to the section heading; the unlinked
 * intro keeps the pinned wording that mentions entering a username and
 * token although no form exists yet; there is no spinner component, so the
 * delayed loading state is text only.
 */

declare global {
	interface Window {
		__passkeyGetCalls?: number;
	}
}

const LINKED = { linked: true, username: 'alice_lb' };
const UNLINKED = { linked: false };
const json = (body: unknown, status = 200) => ({
	status,
	headers: { 'content-type': 'application/json' },
	body: JSON.stringify(body)
});
const REAUTH_REQUIRED = json({ code: 'reauth_required', message: 'fresh session needed', retryable: false }, 401);

interface Traffic {
	gets: number;
	deletes: number;
	reauthStarts: number;
	reauthFinishes: number;
}

/**
 * Route the ListenBrainz endpoints. `get` and `del` produce a response per
 * call index so a test can script fail-then-succeed sequences.
 */
async function routeApi(
	page: Page,
	script: {
		get?: (n: number) => ReturnType<typeof json> | Promise<ReturnType<typeof json>>;
		del?: (n: number) => ReturnType<typeof json>;
	}
): Promise<Traffic> {
	const traffic: Traffic = { gets: 0, deletes: 0, reauthStarts: 0, reauthFinishes: 0 };
	await page.route('**/v1/me/listenbrainz', async (route: Route) => {
		const method = route.request().method();
		if (method === 'GET') {
			traffic.gets += 1;
			await route.fulfill(await (script.get ?? (() => json(LINKED)))(traffic.gets));
		} else if (method === 'DELETE') {
			traffic.deletes += 1;
			await route.fulfill((script.del ?? (() => json(UNLINKED)))(traffic.deletes));
		} else {
			// The link route is not dispatched and the UI must never call it.
			throw new Error(`unexpected ${method} to /v1/me/listenbrainz`);
		}
	});
	await page.route('**/v1/session/reauthenticate/start', async (route) => {
		traffic.reauthStarts += 1;
		await route.fulfill(
			json({ challenge: 'Y2hhbGxlbmdl', allow_credentials: [], rp_id: 'localhost' })
		);
	});
	await page.route('**/v1/session/reauthenticate/finish', async (route) => {
		traffic.reauthFinishes += 1;
		await route.fulfill(json({ ok: true }));
	});
	return traffic;
}

/**
 * `navigator.credentials.get` double. With `hold`, the ceremony stays
 * pending until the page aborts it (then rejects with AbortError, like a
 * real browser); otherwise it resolves with a fixture credential.
 */
async function installPasskeyDouble(page: Page, { hold = false } = {}): Promise<void> {
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
		Object.defineProperty(window, 'PublicKeyCredential', { value: FixturePublicKeyCredential });
		window.__passkeyGetCalls = 0;
		Object.defineProperty(navigator, 'credentials', {
			value: {
				get: (options?: { signal?: AbortSignal }) => {
					window.__passkeyGetCalls = (window.__passkeyGetCalls ?? 0) + 1;
					if (!holdCeremony) return Promise.resolve(new FixturePublicKeyCredential());
					return new Promise((_resolve, reject) => {
						options?.signal?.addEventListener('abort', () =>
							reject(new DOMException('aborted', 'AbortError'))
						);
					});
				}
			}
		});
	}, hold);
}

const section = (page: Page) => page.locator('section[aria-labelledby="settings-listenbrainz-heading"]');
const unlinkButton = (page: Page) => section(page).getByRole('button', { name: 'Unlink' });
const confirmDialog = (page: Page) => page.getByRole('dialog', { name: 'Unlink ListenBrainz?' });
const reauthDialog = (page: Page) => page.getByRole('dialog', { name: "Confirm it's you" });

async function openLinked(page: Page) {
	await page.goto('/settings');
	await expect(section(page).getByText('Linked as')).toBeVisible();
	await expect(section(page).getByText('alice_lb')).toBeVisible();
}

async function startUnlink(page: Page) {
	await unlinkButton(page).click();
	await expect(confirmDialog(page)).toBeVisible();
	await expect(confirmDialog(page).getByRole('button', { name: 'Cancel' })).toBeFocused();
}

test('linked: Cancel on the confirmation sends no DELETE and restores focus; Unlink sends one DELETE', async ({
	page
}) => {
	const traffic = await routeApi(page, {});
	await openLinked(page);
	await expect(section(page)).toContainText(
		"NNTune doesn't send your listens or feedback to ListenBrainz yet. Your NNTune library, likes and history work the same whether or not you link."
	);
	// Token-free and no link form in the pre-PUT build.
	await expect(section(page).locator('input, form')).toHaveCount(0);
	await expect(page.getByRole('button', { name: /change account|link account/i })).toHaveCount(0);

	await startUnlink(page);
	await confirmDialog(page).getByRole('button', { name: 'Cancel' }).click();
	await expect(confirmDialog(page)).toBeHidden();
	await expect(unlinkButton(page)).toBeFocused();
	expect(traffic.deletes).toBe(0);

	await startUnlink(page);
	await confirmDialog(page).getByRole('button', { name: 'Unlink' }).click();
	await expect(section(page).getByText('Linked as')).toHaveCount(0);
	await expect(section(page).getByText('Link your ListenBrainz account to NNTune.', { exact: false })).toBeVisible();
	await expect(page.getByText('ListenBrainz unlinked').first()).toBeVisible();
	await expect(section(page).getByRole('heading', { name: 'ListenBrainz' })).toBeFocused();
	expect(traffic.deletes).toBe(1);
	await expect(unlinkButton(page)).toHaveCount(0);
});

test('unlinked status renders the intro and honesty line with no controls', async ({ page }) => {
	await routeApi(page, { get: () => json(UNLINKED) });
	await page.goto('/settings');
	await expect(section(page).getByText('Link your ListenBrainz account to NNTune.', { exact: false })).toBeVisible();
	await expect(section(page)).toContainText("NNTune doesn't send your listens or feedback");
	await expect(section(page).getByRole('button')).toHaveCount(0);
});

test('status loading text appears only while the read is pending, with no controls', async ({ page }) => {
	await routeApi(page, {
		get: async () => {
			await new Promise((resolve) => setTimeout(resolve, 1500));
			return json(LINKED);
		}
	});
	await page.goto('/settings');
	await expect(section(page).getByText('Loading…')).toBeVisible({ timeout: 1200 });
	await expect(section(page).getByRole('button')).toHaveCount(0);
	await expect(section(page).getByText('Linked as')).toBeVisible();
	await expect(section(page).getByText('Loading…')).toHaveCount(0);
});

test('status error shows the generic block and Retry re-runs the read', async ({ page }) => {
	const traffic = await routeApi(page, {
		get: (n) => (n === 1 ? json({ code: 'internal_error', message: 'x', retryable: true }, 503) : json(LINKED))
	});
	await page.goto('/settings');
	await expect(section(page).getByText("Couldn't load your ListenBrainz link.")).toBeVisible();
	await expect(unlinkButton(page)).toHaveCount(0);
	// Other settings stay usable.
	await expect(page.getByRole('link', { name: 'Open Your Library' })).toBeVisible();
	await section(page).getByRole('button', { name: 'Retry' }).click();
	await expect(section(page).getByText('Linked as')).toBeVisible();
	expect(traffic.gets).toBe(2);
});

test('a generic 401 shows the sign-in prompt and no controls', async ({ page }) => {
	await routeApi(page, { get: () => json({ code: 'unauthorized', message: 'x', retryable: false }, 401) });
	await page.goto('/settings');
	await expect(section(page).getByRole('link', { name: 'Sign in' })).toHaveAttribute('href', '/login');
	await expect(section(page).getByRole('button')).toHaveCount(0);
});

test('going offline while the confirmation is open refuses the DELETE', async ({ page, context }) => {
	const traffic = await routeApi(page, {});
	await openLinked(page);
	await startUnlink(page);

	await context.setOffline(true);
	await confirmDialog(page).getByRole('button', { name: 'Unlink' }).click();

	await expect(confirmDialog(page)).toBeHidden();
	await expect(section(page).getByText("You're offline. Connect to change ListenBrainz.")).toBeVisible();
	await expect(unlinkButton(page)).toBeDisabled();
	await expect(section(page).getByText('Linked as')).toBeVisible();
	expect(traffic.deletes).toBe(0);

	await context.setOffline(false);
	await expect(unlinkButton(page)).toBeEnabled();
});

test('offline at load disables Unlink with the reason', async ({ page, context }) => {
	await routeApi(page, {});
	await openLinked(page);
	await context.setOffline(true);
	await expect(unlinkButton(page)).toBeDisabled();
	await expect(section(page).getByText("You're offline. Connect to change ListenBrainz.")).toBeVisible();
});

test('reauth_required opens the dialog; passkey success retries the DELETE exactly once', async ({ page }) => {
	await installPasskeyDouble(page);
	const traffic = await routeApi(page, { del: (n) => (n === 1 ? REAUTH_REQUIRED : json(UNLINKED)) });
	await openLinked(page);
	await startUnlink(page);
	await confirmDialog(page).getByRole('button', { name: 'Unlink' }).click();

	await expect(reauthDialog(page)).toBeVisible();
	await expect(page.getByText('To change your ListenBrainz link, confirm with your passkey.')).toBeVisible();
	expect(traffic.deletes).toBe(1);

	await reauthDialog(page).getByRole('button', { name: 'Continue with passkey' }).click();
	await expect(section(page).getByText('Linked as')).toHaveCount(0);
	await expect(reauthDialog(page)).toBeHidden();
	expect(traffic.deletes).toBe(2);
	expect(traffic.reauthStarts).toBe(1);
	expect(traffic.reauthFinishes).toBe(1);
	await expect(section(page).getByRole('heading', { name: 'ListenBrainz' })).toBeFocused();
});

test('a second reauth_required after the retry shows text and does not loop', async ({ page }) => {
	await installPasskeyDouble(page);
	const traffic = await routeApi(page, { del: () => REAUTH_REQUIRED });
	await openLinked(page);
	await startUnlink(page);
	await confirmDialog(page).getByRole('button', { name: 'Unlink' }).click();
	await reauthDialog(page).getByRole('button', { name: 'Continue with passkey' }).click();

	await expect(section(page).getByRole('alert')).toHaveText("Couldn't confirm it's you. Try again.");
	await expect(reauthDialog(page)).toBeHidden();
	await expect(section(page).getByText('Linked as')).toBeVisible();
	await expect(unlinkButton(page)).toBeFocused();
	expect(traffic.deletes).toBe(2);
	expect(traffic.reauthFinishes).toBe(1);
});

for (const how of ['Cancel button', 'Escape'] as const) {
	test(`passkey ${how} aborts the ceremony, retries nothing and restores focus`, async ({ page }) => {
		await installPasskeyDouble(page, { hold: true });
		const traffic = await routeApi(page, { del: () => REAUTH_REQUIRED });
		await openLinked(page);
		await startUnlink(page);
		await confirmDialog(page).getByRole('button', { name: 'Unlink' }).click();
		await reauthDialog(page).getByRole('button', { name: 'Continue with passkey' }).click();
		await expect(reauthDialog(page).getByRole('button', { name: 'Waiting for your passkey…' })).toBeVisible();
		await expect(section(page)).toHaveAttribute('aria-busy', 'true');

		if (how === 'Escape') await page.keyboard.press('Escape');
		else await reauthDialog(page).getByRole('button', { name: 'Cancel' }).click();

		await expect(reauthDialog(page)).toBeHidden();
		await expect(unlinkButton(page)).toBeFocused();
		await expect(section(page)).toHaveAttribute('aria-busy', 'false');
		await expect(section(page).getByText('Linked as')).toBeVisible();
		expect(traffic.deletes).toBe(1);
		expect(traffic.reauthFinishes).toBe(0);
	});
}

test('unlink failure and account deletion show their exact alerts', async ({ page }) => {
	const traffic = await routeApi(page, {
		del: (n) =>
			n === 1
				? json({ code: 'internal_error', message: 'x', retryable: true }, 503)
				: json({ code: 'account_deleting', message: 'x', retryable: false }, 409)
	});
	await openLinked(page);
	await startUnlink(page);
	await confirmDialog(page).getByRole('button', { name: 'Unlink' }).click();
	await expect(section(page).getByRole('alert')).toHaveText(
		"Couldn't unlink ListenBrainz. Your link is unchanged — try again."
	);
	await expect(section(page).getByText('Linked as')).toBeVisible();
	await expect(unlinkButton(page)).toBeFocused();

	await startUnlink(page);
	await confirmDialog(page).getByRole('button', { name: 'Unlink' }).click();
	await expect(section(page).getByRole('alert')).toHaveText(
		"Your account is being deleted, so ListenBrainz can't be changed."
	);
	await expect(unlinkButton(page)).toHaveCount(0);
	await expect(section(page).getByText('Linked as')).toBeVisible();
	expect(traffic.deletes).toBe(2);
});

test('linked state and open dialogs have no serious or critical axe violations', async ({ page }) => {
	await routeApi(page, {});
	await openLinked(page);
	const scan = async (label: string) => {
		const results = await new AxeBuilder({ page }).analyze();
		const blocking = results.violations.filter((v) => v.impact === 'serious' || v.impact === 'critical');
		expect(blocking, `${label}: ${blocking.map((v) => v.id).join(', ')}`).toEqual([]);
	};
	await scan('linked');
	await startUnlink(page);
	await scan('confirm dialog');
});
