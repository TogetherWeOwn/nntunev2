import { expect, test } from '@playwright/test';

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
	await page.getByRole('button', { name: 'Continue' }).click();
	await page.getByRole('button', { name: 'Continue' }).click();
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
	await page.getByRole('button', { name: 'Continue' }).click();
	await page.getByRole('button', { name: 'Continue' }).click();
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
