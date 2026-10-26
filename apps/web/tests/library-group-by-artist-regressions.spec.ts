import { expect, test, type Page } from '@playwright/test';

/**
 * Builder-owned regressions complementing the protected grouped-library and
 * bulk-queue acceptance banks. Those banks stay byte-identical.
 *
 * Production changes named by these tests:
 * - Feeding the displayed-tracks bulk action the pre-group visible order
 *   makes the appended queue tail disagree with the grouped rows.
 * - Removing the direct 401/403 grouping reset leaves a recovered account
 *   grouped even though no storage-event auth transition occurred.
 */

const GROUPED_ITEMS = [
	{ id: 'grouped-zulu-first', title: 'Zulu First', artist: 'Zulu' },
	{ id: 'grouped-unknown', title: 'Quiet Track', artist: '   ' },
	{ id: 'grouped-alpha-late', title: 'Alpha Late', artist: 'Alpha' },
	{ id: 'grouped-zulu-second', title: 'Zulu Second', artist: 'zulu' },
	{ id: 'grouped-alpha-first', title: 'Alpha First', artist: 'Alpha' }
];
const GROUPED_ROW_ORDER = [
	'grouped-alpha-late',
	'grouped-alpha-first',
	'grouped-zulu-first',
	'grouped-zulu-second',
	'grouped-unknown'
];

const REFUSAL_ITEMS = [
	{ id: 'refusal-zulu', title: 'Refusal Zulu', artist: 'Zulu' },
	{ id: 'refusal-alpha', title: 'Refusal Alpha', artist: 'Alpha' },
	{ id: 'refusal-unknown', title: 'Refusal Quiet', artist: '   ' }
];
const REPLACEMENT_ITEMS = [
	{ id: 'replacement-zeta', title: 'Replacement Zeta', artist: 'Zeta' },
	{ id: 'replacement-alpha', title: 'Replacement Alpha', artist: 'Alpha' },
	{ id: 'replacement-unknown', title: 'Replacement Quiet', artist: '   ' }
];

type LibraryReply = { status: number; body: unknown };

function groupToggle(page: Page) {
	return page.getByRole('button', { name: 'Group by artist', exact: true });
}

async function expectRowOrder(page: Page, ids: string[]) {
	await expect
		.poll(() =>
			page
				.getByTestId('library-item')
				.evaluateAll((rows) => rows.map((row) => row.getAttribute('data-id')))
		)
		.toEqual(ids);
}

async function expectQueueOrder(page: Page, ids: string[], current: string) {
	await expect
		.poll(() =>
			page
				.getByTestId('queue-item')
				.evaluateAll((rows) => rows.map((row) => row.getAttribute('data-track-id')))
		)
		.toEqual(ids);
	await expect
		.poll(() =>
			page.getByTestId('queue-item').evaluateAll((rows) =>
				rows
					.filter((row) => row.getAttribute('aria-current') === 'true')
					.map((row) => row.getAttribute('data-track-id'))
			)
		)
		.toEqual([current]);
	await expect(page.getByTestId('queue-position')).toContainText(
		`Track ${ids.indexOf(current) + 1} of ${ids.length}`
	);
}

async function mockLibrary(page: Page, reply: LibraryReply) {
	let requests = 0;
	await page.route('**/api/library', (route) => {
		requests += 1;
		return route.fulfill({
			status: reply.status,
			contentType: 'application/json',
			body: JSON.stringify(reply.body)
		});
	});
	return { requests: () => requests };
}

test.beforeEach(async ({ context }) => {
	await context.route('**/*', (route) => {
		const url = new URL(route.request().url());
		if (!['localhost', '127.0.0.1'].includes(url.hostname)) {
			return route.abort('blockedbyclient');
		}
		if (url.pathname.startsWith('/api/')) {
			return route.fulfill({ status: 404, contentType: 'application/json', body: '{}' });
		}
		return route.continue();
	});
});

test('[GA-R01] grouped bulk appends missing tracks in rendered group order after the queue prefix', async ({
	page
}) => {
	const api = await mockLibrary(page, { status: 200, body: GROUPED_ITEMS });
	await page.goto('/library');
	await expectRowOrder(
		page,
		GROUPED_ITEMS.map((item) => item.id)
	);

	const quietRow = page.locator('[data-testid="library-item"][data-id="grouped-unknown"]');
	await quietRow.getByRole('button', { name: 'Quiet Track', exact: true }).click();
	await expectQueueOrder(page, ['grouped-unknown'], 'grouped-unknown');

	await groupToggle(page).click();
	await expect(groupToggle(page)).toHaveAttribute('aria-pressed', 'true');
	await expectRowOrder(page, GROUPED_ROW_ORDER);

	const bulk = page.getByRole('button', {
		name: 'Add displayed tracks to queue',
		exact: true
	});
	await expect(bulk).toBeEnabled();
	await bulk.click();

	await expectQueueOrder(
		page,
		['grouped-unknown', ...GROUPED_ROW_ORDER.filter((id) => id !== 'grouped-unknown')],
		'grouped-unknown'
	);
	await expect(page.getByText('Added 4 tracks to the queue.', { exact: true })).toBeVisible();
	await expect(bulk).toBeDisabled();
	await expect(
		page.getByText('All displayed tracks are already in the queue.', { exact: true })
	).toBeVisible();
	expect(api.requests()).toBe(1);
});

for (const status of [401, 403] as const) {
	test(`[GA-R-${status}] direct HTTP refusal resets grouping before Retry loads replacement tracks`, async ({
		page
	}) => {
		const reply: LibraryReply = { status: 200, body: REFUSAL_ITEMS };
		const api = await mockLibrary(page, reply);
		await page.goto('/library');
		await expectRowOrder(
			page,
			REFUSAL_ITEMS.map((item) => item.id)
		);

		await groupToggle(page).click();
		await expect(groupToggle(page)).toHaveAttribute('aria-pressed', 'true');
		await expect(page.getByRole('heading', { name: 'Alpha', exact: true })).toBeVisible();
		await expect(page.getByRole('heading', { name: 'Zulu', exact: true })).toBeVisible();
		await expect(page.getByRole('heading', { name: 'Unknown artist', exact: true })).toBeVisible();

		// No storage event or navigation: only the direct HTTP-refusal state
		// transition can clear the visit-local grouping choice here.
		reply.status = status;
		reply.body = { error: 'boundary' };
		await page.getByRole('button', { name: 'Refresh', exact: true }).click();
		await expect(page.getByRole('alert')).toBeVisible();
		await expect(page.getByTestId('library-item')).toHaveCount(0);
		await expect(groupToggle(page)).toHaveCount(0);

		reply.status = 200;
		reply.body = REPLACEMENT_ITEMS;
		await page.getByRole('button', { name: 'Retry', exact: true }).click();

		// Leaked grouping would put replacement-alpha first and render
		// Alpha/Zeta/Unknown artist headings for the recovered library.
		await expectRowOrder(
			page,
			REPLACEMENT_ITEMS.map((item) => item.id)
		);
		await expect(groupToggle(page)).toHaveAttribute('aria-pressed', 'false');
		await expect(page.getByRole('heading', { name: 'Alpha', exact: true })).toHaveCount(0);
		await expect(page.getByRole('heading', { name: 'Zeta', exact: true })).toHaveCount(0);
		await expect(page.getByRole('heading', { name: 'Unknown artist', exact: true })).toHaveCount(0);
		await expect(page.getByRole('alert')).toHaveCount(0);
		expect(api.requests()).toBe(3);
	});
}
