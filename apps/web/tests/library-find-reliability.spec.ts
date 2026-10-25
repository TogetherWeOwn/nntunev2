import { expect, test } from '@playwright/test';

/** Builder-owned regressions supplementing the protected acceptance banks. */
const ITEMS = [
	{ id: 'find-private', title: 'Private Previous Account Text' },
	{ id: 'find-beta', title: 'Beta Song' }
];
const REPLACEMENT = [
	{ id: 'find-next-one', title: 'Next Song' },
	{ id: 'find-next-two', title: 'Another Song' }
];

test.beforeEach(async ({ context }) => {
	await context.route('**/*', (route) => {
		const url = new URL(route.request().url());
		if (!['localhost', '127.0.0.1'].includes(url.hostname)) return route.abort('blockedbyclient');
		if (url.pathname.startsWith('/api/')) {
			return route.fulfill({ status: 404, contentType: 'application/json', body: '{}' });
		}
		return route.continue();
	});
});

for (const status of [401, 403] as const) {
	test(`[LF-R-${status}] HTTP refusal clears find text before Retry loads replacement tracks`, async ({
		page
	}) => {
		let reply: { status: number; body: unknown } = { status: 200, body: ITEMS };
		let requests = 0;
		await page.route('**/api/library', (route) => {
			requests += 1;
			return route.fulfill({
				status: reply.status,
				contentType: 'application/json',
				body: JSON.stringify(reply.body)
			});
		});
		await page.goto('/library');
		await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);

		const field = page.getByRole('searchbox', { name: 'Find in loaded tracks', exact: true });
		await expect(field).toBeVisible();
		await field.fill('private previous account text');
		await expect(field).toHaveValue('private previous account text');
		await expect(page.getByTestId('library-item')).toHaveAttribute('data-id', 'find-private');

		// No storage event or navigation: only the HTTP-refusal reset can clear this query.
		reply = { status, body: { error: 'boundary' } };
		await page.getByRole('button', { name: 'Refresh', exact: true }).click();
		await expect(page.getByRole('alert')).toBeVisible();
		await expect(field).toHaveCount(0);
		await expect(page.getByTestId('library-item')).toHaveCount(0);

		reply = { status: 200, body: REPLACEMENT };
		await page.getByRole('button', { name: 'Retry', exact: true }).click();
		await expect(field).toBeVisible();
		await expect(field).toHaveValue('');
		await expect(page.getByTestId('library-item')).toHaveCount(REPLACEMENT.length);
		expect(
			await page.getByTestId('library-item').evaluateAll((rows) =>
				rows.map((row) => row.getAttribute('data-id'))
			)
		).toEqual(REPLACEMENT.map((item) => item.id));
		await expect(page.getByRole('alert')).toHaveCount(0);
		expect(requests).toBe(3);
	});
}

test('[LF-R-HIDDEN] find discloses the exact hidden selection and clears only the disclosure', async ({
	page
}) => {
	await page.route('**/api/library', (route) =>
		route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(ITEMS) })
	);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);

	const privateChoice = page.getByRole('checkbox', {
		name: 'Select Private Previous Account Text',
		exact: true
	});
	const betaChoice = page.getByRole('checkbox', { name: 'Select Beta Song', exact: true });
	await expect(privateChoice).toBeVisible();
	await expect(betaChoice).toBeVisible();
	await privateChoice.check();
	await betaChoice.check();
	const count = page.getByTestId('selection-count');
	const hidden = page.getByTestId('selection-hidden-count');
	await expect(count).toHaveText('2 selected');
	await expect(hidden).toHaveCount(0);

	const field = page.getByRole('searchbox', { name: 'Find in loaded tracks', exact: true });
	await expect(field).toBeVisible();
	await field.fill('beta');
	await expect(page.getByTestId('library-item')).toHaveAttribute('data-id', 'find-beta');
	await expect(privateChoice).toHaveCount(0);
	await expect(betaChoice).toBeChecked();
	await expect(hidden).toHaveText('1 selected track is hidden by this find.');
	await expect(count).toHaveText('2 selected');

	await page.getByRole('button', { name: 'Clear search', exact: true }).click();
	await expect(field).toHaveValue('');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(hidden).toHaveCount(0);
	await expect(count).toHaveText('2 selected');
	await expect(privateChoice).toBeChecked();
	await expect(betaChoice).toBeChecked();
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
});
