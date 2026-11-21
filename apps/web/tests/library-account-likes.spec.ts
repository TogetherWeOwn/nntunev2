import { expect, test, type Page } from '@playwright/test';

const CONTEXT = 'account-likes-fixture';
const KEY = `nntune.library.favorites.v1.${CONTEXT}`;
const TRACK = 'Job_A';
const REJECTED = 'Job_B';
const RELEASE = '00000000-0000-4000-8000-000000000001';
const likedTrack = { kind: 'track', target_id: TRACK, created_at: 1_700_000_000 };
const BINDING = 'A'.repeat(43);
const likes = (items: unknown[] = []) => ({ session_binding: BINDING, items });

async function seedLibrary(page: Page, ids: string[] = [], verified = true): Promise<void> {
	await page.addInitScript(({ context, key, ids, verified, binding }) => {
		if (location.pathname !== '/library/liked') return;
		sessionStorage.setItem('nntune.library.auth-context.v1', context);
		if (verified) sessionStorage.setItem('nntune.library.session-binding.v1', JSON.stringify([context, binding]));
		localStorage.setItem(key, JSON.stringify(ids));
	}, { context: CONTEXT, key: KEY, ids, verified, binding: BINDING });
	await page.route('**/api/library', (route) => route.fulfill({
		headers: { 'x-nntune-session-binding': BINDING },
		json: [{ id: TRACK, title: 'Coral Horizon' }, { id: REJECTED, title: 'Tidal Echo' }]
	}));
	await page.route('**/v1/me/likes/session', (route) => route.fulfill({ json: likes() }));
}

for (const status of [401, 503]) {
	test(`Liked Songs handles a direct ${status}`, async ({ page }) => {
		await seedLibrary(page, [TRACK]);
		await page.route('**/v1/me/likes', (route) => route.fulfill({ status, json: {} }), { times: 1 });
		await page.goto('/library/liked');
		const shelf = page.getByRole('region', { name: 'Liked Songs' });
		if (status === 401) {
			await expect(shelf.getByRole('link', { name: 'Sign in' })).toHaveAttribute('href', '/login');
			await expect(shelf.getByRole('listitem').or(shelf.getByRole('button'))).toHaveCount(0);
			return;
		}
		await expect(shelf.getByRole('alert')).toHaveText('Could not load your liked songs.');
		await page.route('**/v1/me/likes', (route) => route.fulfill({ json: likes([likedTrack,
			{ ...likedTrack, target_id: 'Unlisted' }, { ...likedTrack, kind: 'release', target_id: RELEASE }]) }));
		await shelf.getByRole('button', { name: 'Retry liked songs' }).click();
		await expect(shelf.getByRole('listitem')).toHaveText(['Coral Horizon', 'Unknown track']);
	});
}

for (const status of [200, 503]) {
	test(`sign-out ${status} hides liked rows until the outcome is known`, async ({ page }) => {
		await seedLibrary(page, [TRACK]);
		const stored = () => page.evaluate((key) => localStorage.getItem(key), KEY);
		const finish = Promise.withResolvers<void>();
		await page.route('**/v1/session/logout', async (route) => {
			await finish.promise;
			await route.fulfill({ status, json: { ok: status === 200 } });
		});
		await page.route('**/v1/me/likes', (route) => route.fulfill({ json: likes([likedTrack]) }));
		await page.goto('/library/liked');
		const rows = page.getByRole('region', { name: 'Liked Songs' }).getByRole('listitem')
			.or(page.getByRole('button', { name: 'Remove Coral Horizon from Liked' }));
		await expect(rows).toHaveCount(2);
		await page.getByRole('button', { name: 'Account menu' }).click();
		await page.getByRole('menuitem', { name: 'Sign out' }).click();
		await expect(rows).toHaveCount(0);
		expect(await stored()).toBe(JSON.stringify([TRACK]));
		finish.resolve();
		if (status === 200) {
			await expect(page).toHaveURL(/\/login$/);
			expect(await stored()).toBeNull();
		} else {
			await expect(page.getByText('Could not sign out. Please try again.')).toBeVisible();
			await page.getByRole('button', { name: 'Retry', exact: true }).click();
			await expect(rows).toHaveCount(2);
			expect(await stored()).toBe(JSON.stringify([TRACK]));
		}
	});
}
for (const result of ['partial', 'corrupt', 'all rejected'] as const) {
	test(`explicit import reports ${result} and preserves unconfirmed device data`, async ({ page }) => {
		await seedLibrary(page, [TRACK, REJECTED]);
		const before = result === 'partial' ? JSON.stringify([TRACK, REJECTED, 'New_Job']) : '{broken';
		const accepted = result !== 'all rejected';
		let posts = 0;
		await page.route('**/v1/me/likes', (route) => route.fulfill({ json: likes(posts && accepted ? [likedTrack] : []) }));
		await page.route('**/v1/me/likes/import', async (route) => {
			posts++;
			expect(route.request().postDataJSON()).toEqual({ session_binding: BINDING, items: [TRACK, REJECTED]
				.map((target_id) => ({ kind: 'track', target_id })) });
			await page.evaluate(({ key, value }) => localStorage.setItem(key, value), { key: KEY, value: before });
			const rejected = (accepted ? [REJECTED] : [TRACK, REJECTED]).map((target_id) => ({ target_id, reason: 'not_member' }));
			await route.fulfill({ json: { added: Number(accepted), already: 0, rejected } });
		});
		await page.goto('/library/liked');
		const shelf = page.getByRole('region', { name: 'Liked Songs' });
		await expect(shelf.getByText('No songs saved to this account yet.')).toBeVisible();
		expect(posts).toBe(0);
		await shelf.getByRole('button', { name: 'Import device favorites' }).click();
		await expect(shelf.getByRole('status')).toHaveText({
			partial: 'Some favorites remain on this device.',
			corrupt: 'Favorites were saved to your account, but device cleanup could not finish.',
			'all rejected': 'No device favorites were confirmed by your account.'
		}[result]);
		await expect(shelf.getByText(accepted ? 'Coral Horizon' : 'No songs saved to this account yet.')).toBeVisible();
		if (result === 'partial') await expect(page.getByRole('button', { name: /^Remove .+ from Liked$/ })).toHaveText(['Remove Tidal Echo from Liked']);
		expect(await page.evaluate((key) => localStorage.getItem(key), KEY)).toBe(result === 'partial' ? JSON.stringify([REJECTED, 'New_Job']) : before);
	});
}

test('an unbound legacy list stays device-only even when the shelf has verified titles', async ({ page }) => {
	await seedLibrary(page, [TRACK], false);
	await page.route('**/v1/me/likes', (route) => route.fulfill({ json: likes([likedTrack]) }));
	await page.goto('/library/liked');
	const shelf = page.getByRole('region', { name: 'Liked Songs' });
	await expect(shelf.getByRole('listitem')).toHaveText('Coral Horizon');
	await expect(page.getByRole('button', { name: 'Remove Coral Horizon from Liked' })).toBeVisible();
	await expect(shelf.getByRole('button', { name: 'Import device favorites' })).toHaveCount(0);
	expect(await page.evaluate((key) => localStorage.getItem(key), KEY)).toBe(JSON.stringify([TRACK]));
});
for (const cause of ['shelf overlap', 'retirement overlap', 'shelf mismatch', 'retirement mismatch'] as const) {
	test(`an old receipt cannot publish or retire after ${cause}`, async ({ page }) => {
		await seedLibrary(page, [TRACK]);
		const checking = Promise.withResolvers<void>(), finish = Promise.withResolvers<void>();
		const importing = cause.startsWith('retirement'), overlap = cause.endsWith('overlap');
		let verifies = 0;
		await page.route('**/v1/me/likes/session', async (route) => {
			const held = ++verifies === (importing ? 3 : 2);
			if (held) { checking.resolve(); await finish.promise; }
			return route.fulfill({ json: { session_binding: held && !overlap ? 'E'.repeat(43) : BINDING } });
		});
		await page.route('**/v1/me/likes', (route) => route.fulfill({ json: likes([likedTrack]) }));
		await page.route('**/v1/me/likes/import', (route) => route.fulfill({ json: { added: 0, already: 1, rejected: [] } }));
		await page.goto('/library/liked');
		if (importing) await page.getByRole('button', { name: 'Import device favorites' }).click();
		await checking.promise;
		// The writing context does not receive a storage event for its own change.
		if (overlap) await page.evaluate(() => navigator.locks.request('nntune.auth.cookie-transition.v1',
			() => localStorage.setItem('nntune.auth.generation.v1', 'new-generation')));
		finish.resolve();
		const main = page.getByRole('main');
		await expect(overlap ? main.getByRole('button', { name: 'Retry', exact: true }) :
			main.getByRole('link', { name: 'Sign in' })).toBeVisible();
		await expect(page.getByText('Coral Horizon')).toHaveCount(0);
		expect(await page.evaluate((key) => localStorage.getItem(key), KEY)).toBe(JSON.stringify([TRACK]));
	});
}
