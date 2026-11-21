import { expect, test } from '@playwright/test';

const path = '**/v1/plays/export';
const envelope = (events: unknown[]) => ({ events, next_cursor: null, retention: 'account_lifetime' });

test.beforeEach(async ({ page }) => {
	await page.route('**/api/library', (route) => route.fulfill({ json: [] }));
});

test('Home keeps a failed history read recoverable and distinguishes empty history from refusal', async ({ page }) => {
	const shelf = page.getByRole('region', { name: 'Recent listens' });
	await page.route(path, (route) => route.fulfill({ status: 503 }), { times: 1 });
	await page.goto('/home');
	await expect(shelf.getByRole('status')).toHaveText('Could not load listening history.');
	await expect(shelf.getByRole('listitem')).toHaveCount(0);

	await page.route(path, (route) => route.fulfill({ json: envelope([]) }), { times: 1 });
	await shelf.getByRole('button', { name: 'Retry' }).click();
	await expect(shelf.getByText('No listening events have been recorded for this account.')).toBeVisible();
	await expect(shelf.getByRole('listitem')).toHaveCount(0);

	await page.route(path, (route) => route.fulfill({ status: 403 }), { times: 1 });
	await page.reload();
	await expect(shelf.getByRole('alert')).toHaveText('You do not have access to this listening history.');
	await expect(shelf.getByRole('listitem')).toHaveCount(0);
});

test('Home sends browser Origin and discards already rendered listens on a cross-tab session change', async ({ page, context }) => {
	const shelf = page.getByRole('region', { name: 'Recent listens' });
	await page.route(path, async (route) => {
		const request = route.request();
		expect(request.method()).toBe('POST');
		expect(await request.headerValue('origin')).toBe(new URL(request.url()).origin);
		expect(await request.headerValue('authorization')).toBeNull();
		expect(request.postDataJSON()).toEqual({});
		await route.fulfill({ json: envelope([{
			playback_id: '10000000-0000-0000-0000-000000000000', track_id: 'outgoing-track',
			event: 'complete', recorded_at: 1_700_000_000
		}]) });
	}, { times: 1 });
	await page.goto('/home');
	await expect(shelf.getByRole('listitem')).toContainText('Playback completed · Track reference: outgoing-track');
	await expect(shelf.locator('time')).toHaveAttribute('datetime', '2023-11-14T22:13:20.000Z');

	const otherTab = await context.newPage();
	await otherTab.goto('/robots.txt');
	await otherTab.evaluate(() => localStorage.setItem('nntune.auth.generation.v1', crypto.randomUUID()));
	await expect(shelf.getByRole('status')).toContainText('Listening history is hidden');
	await expect(shelf.getByRole('listitem')).toHaveCount(0);

	await page.route(path, (route) => route.fulfill({ status: 401 }), { times: 1 });
	await shelf.getByRole('button', { name: 'Retry' }).click();
	await expect(shelf.getByRole('link', { name: 'Sign in' })).toHaveAttribute('href', '/login');
	await expect(shelf.getByRole('listitem')).toHaveCount(0);
});
