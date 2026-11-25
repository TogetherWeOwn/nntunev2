import { expect, test } from '@playwright/test';
import { API } from '../../../e2e/helpers/contract.js';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import type { IssuedInvite, RegisterStartResponse } from '../src/lib/api/client.js';

const { signup, library, player } = PRIVATE_MUSIC;
const registerStart = API.authStarts[0];
const email = 'expired-listener@example.test';
const now = 1_800_000_000;
const invite: IssuedInvite = {
	code: Buffer.from('expired-invite!!').toString('base64url'),
	expires_at: now - 1
};

test.use({ serviceWorkers: 'block' });

test('expired private invite is refused without opening Library or requesting a play', async ({
	page, context, baseURL
}) => {
	if (!baseURL || !['localhost', '127.0.0.1', '[::1]'].includes(new URL(baseURL).hostname)) {
		throw new Error('Expired-invite signup requires a loopback browser fixture');
	}
	const origin = new URL(baseURL).origin;
	const signupUrl = new URL(signup.path, origin);
	const loginUrl = new URL('/login', origin).href;
	const inviteUrl = new URL(signupUrl);
	inviteUrl.hash = new URLSearchParams({ invite: invite.code }).toString();
	const starts: unknown[] = [];
	const unexpectedRequests: string[] = [];
	await context.route('**/*', async (route) => {
		const request = route.request();
		const { origin: target, pathname } = new URL(request.url());
		if (target === origin && pathname === registerStart && request.method() === 'POST') {
			const body = request.postDataJSON();
			starts.push(body);
			// The Worker tests the expiry decision. Here the real form receives
			// its normal generic auth refusal (or the normal challenge for a fresh fixture).
			if (body?.invite_code !== invite.code || invite.expires_at <= now) {
				return route.fulfill({ status: 401, json: { error: 'check your email and try again' } });
			}
			const challenge: RegisterStartResponse = {
				challenge: 'Zmlyc3Q=', user_id: 'dXNlcg', rp_id: signupUrl.hostname
			};
			return route.fulfill({ json: challenge });
		}
		if (pathname === library.path || pathname === library.api || pathname === player.plays ||
			pathname.startsWith(`${player.plays}/`) || pathname === '/v1/me/private-session' ||
			pathname.startsWith('/v1/auth/passkey/register/')) {
			unexpectedRequests.push(`${request.method()} ${pathname}`);
			return route.abort();
		}
		return target === origin && request.method() === 'GET' ? route.continue() : route.abort();
	});

	const laterNavigations: string[] = [];
	page.on('framenavigated', (frame) => {
		if (frame === page.mainFrame()) laterNavigations.push(frame.url());
	});
	// Capture the entry count before the app handles the invite. A push
	// would leave another entry behind even if the current URL is clean.
	await page.addInitScript(() => {
		Object.defineProperty(window, '__signupInitialHistoryLength', { value: history.length });
	});
	await page.goto(inviteUrl.href);
	const inviteCode = page.getByRole('textbox', { name: 'Invite code (if you have one)' });
	await expect(inviteCode).toHaveValue(invite.code);
	await expect(page).toHaveURL(signupUrl.href);
	expect(await page.evaluate(() => history.length - Reflect.get(window, '__signupInitialHistoryLength'))).toBe(0);
	const subsequentNavigations = laterNavigations.length;
	await page.getByTestId(signup.email).fill(email);
	await page.getByTestId(signup.submit).click();
	await expect(page.getByRole('alert')).toHaveText('Something went wrong. Try again.');
	await expect(page.getByTestId(signup.submit)).toBeEnabled();
	await expect(page.getByLabel(signup.emailCode, { exact: true })).toHaveCount(0);
	await expect(page).toHaveURL(signupUrl.href);
	expect(await page.evaluate(() => JSON.stringify(history.state))).not.toContain(invite.code);

	await page.getByRole('link', { name: 'Sign in', exact: true }).click();
	await expect(page).toHaveURL(loginUrl);
	await page.goBack();
	await expect(page).toHaveURL(signupUrl.href);
	await expect(inviteCode).toHaveValue('');

	expect(starts).toEqual([{ email, invite_code: invite.code }]);
	expect(unexpectedRequests).toEqual([]);
	expect(laterNavigations.slice(subsequentNavigations)).toEqual([loginUrl, signupUrl.href]);
});
