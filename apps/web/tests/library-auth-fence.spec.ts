import { expect, test } from '@playwright/test';

const current = 'A'.repeat(43);

test('a closed writer allows Retry, an outage preserves playback, and a replacement clears the queue and Undo', async ({ page, context }) => {
	let binding = current;
	let status = 200;
	let verifies = 0;
	await page.route('**/api/library', (r) => r.fulfill({ headers: { 'x-nntune-session-binding': binding }, json: [{ id: 'track', title: 'Scoped track' }] }));
	await page.route('**/v1/me/likes/session', (r) => { verifies++; return r.fulfill({ status, json: { session_binding: current } }); });
	await page.addInitScript((binding) => {
		sessionStorage.setItem('nntune.library.auth-context.v1', 'bound');
		sessionStorage.setItem('nntune.library.session-binding.v1', JSON.stringify(['bound', binding]));
		localStorage.setItem('nntune.library.favorites.v1.bound', '["track"]');
	}, current);
	const writer = await context.newPage();
	await writer.goto('/login');
	await writer.evaluate(() => new Promise<void>((ready) => {
		void navigator.locks.request('nntune.auth.cookie-transition.v1', () => { ready(); return new Promise(() => {}); });
	}));
	await page.goto('/library');
	const retry = page.getByRole('button', { name: 'Retry', exact: true });
	await expect(retry).toBeVisible();
	await writer.close();
	await retry.click();
	await page.getByTestId('library-item').click();
	await expect(page.getByTestId('now-playing')).toContainText('Scoped track');
	status = 503;
	await page.getByRole('link', { name: 'Liked', exact: true }).click();
	await expect(retry).toBeVisible();
	await expect(page.getByTestId('now-playing')).toContainText('Scoped track');
	status = 200;
	await retry.click();
	await page.getByRole('button', { name: 'Remove Scoped track from Liked' }).click();
	const undo = page.getByRole('button', { name: 'Undo', exact: true });
	await expect(undo).toBeVisible();
	status = 503;
	binding = 'E'.repeat(43);
	await page.getByRole('link', { name: 'Back to Your Library' }).click();
	await expect(page.getByRole('alert')).toContainText('Your session changed');
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('library-item')).toHaveCount(0);
	await expect(undo).toHaveCount(0);
	expect(verifies).toBe(3);
});
