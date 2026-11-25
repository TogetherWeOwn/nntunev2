import { expect, test } from '@playwright/test';
import { API } from '../../../e2e/helpers/contract.js';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import type { IssuedInvite, RegisterStartResponse } from '../src/lib/api/client.js';

const { signup, library } = PRIVATE_MUSIC;
const registerStart = API.authStarts[0];

test.use({ serviceWorkers: 'block' });

test('private invite expires after opening signup and before submitting', async ({
	page, context, baseURL
}) => {
	if (!baseURL || !['localhost', '127.0.0.1', '[::1]'].includes(new URL(baseURL).hostname)) {
		throw new Error('Invite expiry signup requires a loopback browser fixture');
	}
	const signupUrl = new URL(signup.path, baseURL);
	const invite: IssuedInvite = {
		code: Buffer.from('invite-clock-web').toString('base64url'),
		expires_at: 1_800_000_000
	};
	const inviteUrl = new URL(signupUrl);
	inviteUrl.hash = new URLSearchParams({ invite: invite.code }).toString();
	const email = 'listener@example.test';
	let now = invite.expires_at - 1;
	const starts: { at: number; body: unknown }[] = [];
	const unexpectedRequests: string[] = [];

	await context.route('**/*', async (route) => {
		const request = route.request();
		const { origin, pathname } = new URL(request.url());
		const method = request.method();
		if (origin === signupUrl.origin && pathname === registerStart && method === 'POST') {
			const body = request.postDataJSON();
			starts.push({ at: now, body });
			// The API decides expiry at submission, with the cutoff second excluded.
			if (body?.invite_code !== invite.code || invite.expires_at <= now) {
				return route.fulfill({ status: 401, json: { error: 'check your email and try again' } });
			}
			const challenge: RegisterStartResponse = {
				challenge: 'Zmlyc3Q=', user_id: 'dXNlcg', rp_id: signupUrl.hostname
			};
			return route.fulfill({ json: challenge });
		}
		if (origin !== signupUrl.origin || method !== 'GET' ||
			pathname.startsWith('/v1/') || pathname.startsWith('/api/') ||
			pathname === library.path || pathname === '/account/billing') {
			unexpectedRequests.push(`${method} ${pathname}`);
			return route.abort();
		}
		return route.continue();
	});

	await page.goto(inviteUrl.href);
	await expect(page.getByLabel('Invite code (if you have one)')).toHaveValue(invite.code);
	await page.getByLabel('Email', { exact: true }).fill(email);
	const submit = page.getByRole('button', { name: 'Create account with passkey' });
	await expect(submit).toBeEnabled();
	expect(now).toBeLessThan(invite.expires_at);
	expect(starts).toEqual([]);

	now += 1;
	const [response] = await Promise.all([
		page.waitForResponse((candidate) => candidate.url() === new URL(registerStart, signupUrl).href &&
			candidate.request().method() === 'POST'),
		submit.click()
	]);
	await expect(page.getByRole('alert')).toHaveText('Something went wrong. Try again.');
	expect(response.status()).toBe(401);
	await expect(page.getByLabel(signup.emailCode, { exact: true })).toHaveCount(0);
	await expect(page).toHaveURL((url) => url.origin === signupUrl.origin && url.pathname === signup.path);
	expect(starts).toEqual([{ at: invite.expires_at, body: { email, invite_code: invite.code } }]);
	expect(unexpectedRequests).toEqual([]);
});
