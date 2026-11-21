import { expect, test, type Page } from '@playwright/test';

const CONTEXT = 'account-likes-fixture';
const KEY = `nntune.library.favorites.v1.${CONTEXT}`;
const TRACK = 'Job_A';
const REJECTED = 'Job_B';
const RELEASE = '00000000-0000-4000-8000-000000000001';
const likedTrack = { kind: 'track', target_id: TRACK, created_at: 1_700_000_000 };
const BINDING = 'fixture-session-a';
const likes = (items: unknown[] = []) => ({ session_binding: BINDING, items });

async function seedLibrary(page: Page, ids: string[] = []): Promise<void> {
	await page.addInitScript(({ context, key, ids }) => {
		sessionStorage.setItem('nntune.library.auth-context.v1', context);
		localStorage.setItem(key, JSON.stringify(ids));
	}, { context: CONTEXT, key: KEY, ids });
	await page.route('**/api/library', (route) => route.fulfill({
		headers: { 'x-nntune-session-binding': BINDING },
		json: [{ id: TRACK, title: 'Coral Horizon' }, { id: REJECTED, title: 'Tidal Echo' }]
	}));
	await page.route('**/v1/me/likes/session', (route) => route.fulfill({ json: likes() }));
}

test('Liked Songs can retry a failed read and only lists confirmed track likes', async ({ page }) => {
	await seedLibrary(page);
	let reads = 0;
	await page.route('**/v1/me/likes', (route) => ++reads === 1
		? route.fulfill({ status: 503, json: {} })
		: route.fulfill({ json: likes([likedTrack, { ...likedTrack, target_id: 'Unlisted' },
			{ ...likedTrack, kind: 'release', target_id: RELEASE }]) }));
	await page.goto('/library/liked');
	const shelf = page.getByRole('region', { name: 'Liked Songs' });
	await expect(shelf.getByRole('alert')).toHaveText('Could not load your liked songs.');
	await shelf.getByRole('button', { name: 'Retry liked songs' }).click();
	await expect(shelf.getByRole('listitem')).toHaveText(['Coral Horizon', 'Unknown track']);
});

for (const corrupt of [false, true]) {
	test(`explicit import preserves ${corrupt ? 'corrupt storage' : 'rejected and concurrently added favorites'}`, async ({ page }) => {
		await seedLibrary(page, [TRACK, REJECTED]);
		const before = JSON.stringify([TRACK, REJECTED, 'New_Job']);
		let posts = 0;
		await page.route('**/v1/me/likes', (route) => route.fulfill({ json: likes(posts ? [likedTrack] : []) }));
		await page.route('**/v1/me/likes/import', async (route) => {
			posts++;
			expect(route.request().postDataJSON()).toEqual({ session_binding: BINDING, items: [TRACK, REJECTED]
				.map((target_id) => ({ kind: 'track', target_id })) });
			await page.evaluate(({ key, value }) => localStorage.setItem(key, value), { key: KEY, value: corrupt ? '{broken' : before });
			await route.fulfill({ json: { added: 1, already: 0, rejected: [{ target_id: REJECTED, reason: 'not_member' }] } });
		});
		await page.goto('/library/liked');
		const shelf = page.getByRole('region', { name: 'Liked Songs' });
		await expect(shelf.getByText('No songs saved to this account yet.')).toBeVisible();
		expect(posts).toBe(0);
		await shelf.getByRole('button', { name: 'Import device favorites' }).click();
		await expect(shelf.getByText('Coral Horizon')).toBeVisible();
		await expect(shelf.getByRole('status')).toHaveText(corrupt
			? 'Favorites were saved to your account, but device cleanup could not finish.'
			: 'Some favorites remain on this device.');
		expect(await page.evaluate((key) => localStorage.getItem(key), KEY)).toBe(corrupt ? '{broken' : JSON.stringify([REJECTED, 'New_Job']));
	});
}

for (const endpoint of ['likes', 'likes/import']) {
	test(`a late ${endpoint} response cannot publish or retire after the session changes`, async ({ page }) => {
		await seedLibrary(page, [TRACK]);
		let binding = BINDING;
		await page.route('**/v1/me/likes/session', (route) => route.fulfill({ json: { session_binding: binding } }));
		await page.route('**/v1/me/likes', (route) => route.fulfill({ json: likes([likedTrack]) }));
		let release!: () => void;
		const delayed = new Promise<void>((resolve) => { release = resolve; });
		await page.route(`**/v1/me/${endpoint}`, async (route) => {
			await delayed;
			// The response belongs to the earlier session; no storage event has arrived.
			binding = 'fixture-session-b';
			return route.fulfill({ json: endpoint === 'likes' ? likes([likedTrack]) : { added: 0, already: 1, rejected: [] } });
		});
		await page.goto('/library/liked');
		if (endpoint.endsWith('import')) await page.getByRole('button', { name: 'Import device favorites' }).click();
		else await expect(page.getByRole('button', { name: 'Remove Coral Horizon from Liked' })).toBeVisible();
		release();
		await expect(page.getByRole('main').getByRole('link', { name: 'Sign in' })).toBeVisible();
		await expect(page.getByText('Coral Horizon')).toHaveCount(0);
		expect(await page.evaluate((key) => localStorage.getItem(key), KEY)).toBe(JSON.stringify([TRACK]));
	});
}
