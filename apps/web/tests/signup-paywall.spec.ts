import { expect, test } from '@playwright/test';

test('signup start limit shows paywall without a duplicate inline alert', async ({ page }) => {
	await page.route('**/v1/auth/passkey/register/start', (route) =>
		route.fulfill({
			status: 402,
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify({ code: 'plan_limit_exceeded' })
		})
	);

	await page.goto('/signup');
	await page.getByTestId('signup-email').fill('alice@example.com');
	await page.getByTestId('signup-submit').click();

	const dialog = page.getByRole('dialog');
	await expect(dialog).toBeVisible();
	await expect(page.getByRole('alert')).toHaveCount(0);
	await expect(page.getByTestId('signup-email')).toBeVisible();
	await expect(page.getByTestId('signup-email')).toHaveValue('alice@example.com');
	await expect(dialog.getByRole('link', { name: /upgrade your plan/i })).toHaveAttribute(
		'href',
		'/pricing'
	);
});

test('signup finish limit preserves the code and challenge for retry', async ({ page }) => {
	await page.addInitScript(() => {
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
		Object.defineProperty(navigator, 'credentials', {
			value: { create: async () => new FixturePublicKeyCredential() }
		});
	});
	await page.route('**/v1/auth/passkey/register/start', (route) =>
		route.fulfill({
			status: 200,
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify({ challenge: 'Y2hhbGxlbmdl', user_id: 'dXNlcg', rp_id: 'localhost' })
		})
	);
	let finishPosts = 0;
	const finishBodies: Array<Record<string, unknown>> = [];
	await page.route('**/v1/auth/passkey/register/finish', async (route) => {
		finishPosts += 1;
		finishBodies.push(route.request().postDataJSON() as Record<string, unknown>);
		if (finishPosts === 1) {
			await route.fulfill({
				status: 429,
				headers: { 'content-type': 'application/json', 'Retry-After': '300' },
				body: JSON.stringify({ code: 'rate_limited' })
			});
		} else {
			await route.fulfill({
				status: 200,
				headers: { 'content-type': 'application/json' },
				body: JSON.stringify({ ok: true })
			});
		}
	});

	await page.goto('/signup');
	await page.getByTestId('signup-email').fill('alice@example.com');
	await page.getByTestId('signup-submit').click();
	await page.getByLabel('Email code').fill('fixture-code');
	await page.getByRole('button', { name: /finish creating account/i }).click();

	const dialog = page.getByRole('dialog');
	await expect(dialog).toBeVisible();
	await expect(page.getByRole('alert')).toHaveCount(0);
	await expect(page.getByLabel('Email code')).toBeVisible();
	await expect(page.getByLabel('Email code')).toHaveValue('fixture-code');
	await expect(dialog.getByRole('link', { name: /upgrade your plan/i })).toHaveAttribute(
		'href',
		'/pricing'
	);
	await expect(page.getByTestId('paywall-countdown')).toContainText(/try again/i);

	await dialog.getByRole('button', { name: 'Not now' }).click();
	await expect(dialog).toBeHidden();
	await page.getByRole('button', { name: /finish creating account/i }).click();
	await page.waitForURL('/onboarding/byop');
	expect(finishPosts).toBe(2);
	expect(finishBodies[1]).toEqual(finishBodies[0]);
	expect(finishBodies[1]).toMatchObject({
		challenge: 'Y2hhbGxlbmdl',
		magic_token: 'fixture-code'
	});
});
