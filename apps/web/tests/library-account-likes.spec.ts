import { expect, test, type Page } from '@playwright/test';

const CONTEXT = 'account-likes-fixture';
const KEY = `nntune.library.favorites.v1.${CONTEXT}`;
const TRACK = 'Job_A';
const REJECTED = 'Job_B';
const RELEASE = '00000000-0000-4000-8000-000000000001';
const likedTrack = { kind: 'track', target_id: TRACK, created_at: 1_700_000_000 };

async function seedLibrary(page: Page, ids: string[] = []): Promise<void> {
	await page.addInitScript(({ context, key, ids }) => {
		sessionStorage.setItem('nntune.library.auth-context.v1', context);
		localStorage.setItem(key, JSON.stringify(ids));
	}, { context: CONTEXT, key: KEY, ids });
	await page.route('**/api/library', (route) => route.fulfill({ json: [
		{ id: TRACK, title: 'Coral Horizon' },
		{ id: REJECTED, title: 'Tidal Echo' },
		{ id: RELEASE, title: 'Not a liked track' }
	] }));
}

test('Liked Songs can retry a failed read and only lists confirmed track likes', async ({ page }) => {
	await seedLibrary(page);
	let reads = 0;
	await page.route('**/v1/me/likes', (route) => ++reads === 1
		? route.fulfill({ status: 503, json: {} })
		: route.fulfill({ json: { items: [likedTrack, { ...likedTrack, kind: 'release', target_id: RELEASE }] } }));
	await page.goto('/library/liked');

	const shelf = page.getByRole('region', { name: 'Liked Songs' });
	await expect(shelf.getByRole('alert')).toHaveText('Could not load your liked songs.');
	await shelf.getByRole('button', { name: 'Retry liked songs' }).click();
	await expect(shelf.getByRole('listitem')).toHaveText(['Coral Horizon']);
});

test('explicit import keeps rejected and concurrently added device favorites', async ({ page }) => {
	await seedLibrary(page, [TRACK, REJECTED]);
	let imported = false;
	let posts = 0;
	await page.route('**/v1/me/likes', (route) => route.fulfill({ json: { items: imported ? [likedTrack] : [] } }));
	await page.route('**/v1/me/likes/import', async (route) => {
		posts++;
		expect(route.request().postDataJSON()).toEqual({ items: [
			{ kind: 'track', target_id: TRACK }, { kind: 'track', target_id: REJECTED }
		] });
		await page.evaluate(({ key, ids }) => localStorage.setItem(key, JSON.stringify(ids)), {
			key: KEY, ids: [TRACK, REJECTED, 'New_Job']
		});
		imported = true;
		await route.fulfill({ json: { added: 1, already: 0, rejected: [{ target_id: REJECTED, reason: 'not_member' }] } });
	});
	await page.goto('/library/liked');

	const shelf = page.getByRole('region', { name: 'Liked Songs' });
	const button = shelf.getByRole('button', { name: 'Import device favorites' });
	await expect(button).toBeEnabled();
	await expect(shelf.getByText('No songs saved to this account yet.')).toBeVisible();
	expect(posts).toBe(0);
	await button.click();
	await expect(shelf.getByRole('listitem')).toHaveText(['Coral Horizon']);
	await expect(shelf.getByRole('status')).toHaveText('Some favorites remain on this device.');
	await expect(page.getByRole('button', { name: 'Remove Coral Horizon from Liked' })).toHaveCount(0);
	await expect(page.getByRole('button', { name: 'Remove Tidal Echo from Liked' })).toBeVisible();
	expect(await page.evaluate((key) => JSON.parse(localStorage.getItem(key)!), KEY)).toEqual([REJECTED, 'New_Job']);
});

test('an import receipt arriving after an auth transition cannot retire device data', async ({ page }) => {
	await seedLibrary(page, [TRACK]);
	await page.route('**/v1/me/likes', (route) => route.fulfill({ json: { items: [likedTrack] } }));
	await page.addInitScript(() => {
		const original = window.fetch.bind(window);
		window.fetch = (input, init) => {
			if (typeof input !== 'string' || !input.endsWith('/v1/me/likes/import')) return original(input, init);
			// Model an in-flight write whose receipt arrives despite cancellation.
			return new Promise<Response>((resolve) => {
				window.addEventListener('finish-likes-import', () => {
					resolve(Response.json({ added: 0, already: 1, rejected: [] }));
				}, { once: true });
			});
		};
	});
	await page.goto('/library/liked');
	const shelf = page.getByRole('region', { name: 'Liked Songs' });
	await shelf.getByRole('button', { name: 'Import device favorites' }).click();
	await expect(shelf.getByRole('button', { name: 'Importing…' })).toBeDisabled();
	await page.evaluate(() => new Promise<void>((resolve) => {
		window.dispatchEvent(new StorageEvent('storage', { key: 'nntune.library.auth-transition.v1' }));
		window.dispatchEvent(new Event('finish-likes-import'));
		const channel = new MessageChannel();
		channel.port1.onmessage = () => {
			channel.port1.close();
			channel.port2.close();
			resolve();
		};
		channel.port2.postMessage(null);
	}));
	await expect(page.getByRole('main').getByRole('link', { name: 'Sign in' })).toBeVisible();
	await expect(page.getByText('Coral Horizon')).toHaveCount(0);
	expect(await page.evaluate((key) => JSON.parse(localStorage.getItem(key)!), KEY)).toEqual([TRACK]);
});
