import { expect, test } from '@playwright/test';

test('a closed writer unblocks verified Library; a changed binding clears the active queue', async ({ page, context }) => {
	let binding = 'current';
	let verifies = 0;
	await page.route('**/api/library', (route) => route.fulfill({
		headers: { 'x-nntune-session-binding': binding }, json: [{ id: 'track', title: 'Scoped track' }]
	}));
	await page.route('**/v1/me/likes/session', (route) => {
		verifies++;
		return binding === 'current' ? route.fulfill({ json: { session_binding: binding } }) : route.fulfill({ status: 503 });
	});
	const writer = await context.newPage();
	await writer.goto('/login');
	await writer.evaluate(() => new Promise<void>((ready) => {
		void navigator.locks.request('nntune.auth.cookie-transition.v1', () => { ready(); return new Promise(() => {}); });
	}));
	await page.goto('/library');
	await expect(page.getByRole('button', { name: 'Retry', exact: true })).toBeVisible();
	expect(verifies).toBe(0);
	await writer.close();
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await page.getByTestId('library-item').click();
	await expect(page.getByTestId('now-playing')).toContainText('Scoped track');
	binding = 'replacement';
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.getByRole('alert')).toContainText('Your session changed');
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('library-item')).toHaveCount(0);
	expect(verifies).toBe(1);
});
