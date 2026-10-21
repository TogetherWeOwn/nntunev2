import { expect, test, type Page } from '@playwright/test';

const SNAPSHOT_KEY = 'nntune.library.snapshot.v1';
const AUTH_CONTEXT_KEY = 'nntune.library.auth-context.v1';

const DUPLICATE_ROWS = [
	{ id: 'track-dup-1', title: 'First Copy Song', artist: 'Dup Artist' },
	{ id: 'track-dup-2', title: 'Second Song' },
	{ id: 'track-dup-1', title: 'Conflicting Duplicate Title', artist: 'Other Artist' },
	{ id: 'track-dup-2', title: 'Second Song Again' }
];

function watchPageErrors(page: Page): Error[] {
	const errors: Error[] = [];
	page.on('pageerror', (error) => errors.push(error));
	return errors;
}

async function readSnapshotIds(page: Page): Promise<string[]> {
	const raw = await page.evaluate(
		({ key, contextKey }) => {
			const context = window.sessionStorage.getItem(contextKey);
			return context ? window.sessionStorage.getItem(`${key}.${context}`) : null;
		},
		{ key: SNAPSHOT_KEY, contextKey: AUTH_CONTEXT_KEY }
	);
	expect(raw, 'duplicate response did not persist a snapshot').not.toBeNull();
	return (JSON.parse(raw as string).items as { id: string }[]).map((i) => i.id);
}

test('a duplicate API response renders unique rows without a page error, then refresh still works', async ({
	page
}) => {
	const pageErrors = watchPageErrors(page);
	let refreshed = false;
	await page.route('**/api/library', (route) => {
		const body = refreshed
			? [{ id: 'track-after-refresh', title: 'After Refresh Song' }]
			: DUPLICATE_ROWS;
		refreshed = true;
		return route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(body)
		});
	});

	await page.goto('/library');

	await expect(page.getByTestId('library-list')).toBeVisible();
	await expect(page.getByTestId('library-item')).toHaveCount(2);
	await expect(page.getByTestId('library-list')).toContainText('First Copy Song');
	// The conflicting duplicate row is dropped whole, never merged or rendered.
	await expect(page.getByTestId('library-list')).not.toContainText('Conflicting Duplicate Title');
	await expect(page.getByTestId('library-list')).not.toContainText('Second Song Again');
	expect(pageErrors, 'duplicate keys raised a page error').toEqual([]);
	// The persisted snapshot that a later stale render restores is already unique.
	expect(await readSnapshotIds(page)).toEqual(['track-dup-1', 'track-dup-2']);

	await page.getByRole('button', { name: 'Refresh' }).click();

	await expect(page.getByTestId('library-item')).toHaveCount(1);
	await expect(page.getByTestId('library-list')).toContainText('After Refresh Song');
	expect(pageErrors).toEqual([]);
});

test('a duplicate { items } envelope renders unique rows without a page error', async ({ page }) => {
	const pageErrors = watchPageErrors(page);
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({ items: DUPLICATE_ROWS })
		})
	);

	await page.goto('/library');

	await expect(page.getByTestId('library-item')).toHaveCount(2);
	await expect(page.getByTestId('library-list')).toContainText('First Copy Song');
	expect(pageErrors).toEqual([]);
});

test('duplicates stay unique when a failed refresh falls back to the stale snapshot', async ({
	page
}) => {
	const pageErrors = watchPageErrors(page);
	let failNext = false;
	await page.route('**/api/library', (route) => {
		if (failNext) return route.abort('failed');
		failNext = true;
		return route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(DUPLICATE_ROWS)
		});
	});

	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(2);

	await page.getByRole('button', { name: 'Refresh' }).click();

	await expect(page.getByTestId('library-stale-badge')).toBeVisible();
	await expect(page.getByTestId('library-item')).toHaveCount(2);
	await expect(page.getByTestId('library-list')).toContainText('First Copy Song');
	await expect(page.getByTestId('library-list')).not.toContainText('Conflicting Duplicate Title');
	expect(pageErrors).toEqual([]);
});

test('a seeded duplicate snapshot is still not rendered before the session is verified (control)', async ({
	page
}) => {
	// Duplicate normalization must not change auth/storage isolation: a
	// pre-seeded snapshot (duplicates included) stays hidden while the
	// first fetch cannot verify the session. Snapshot-row uniqueness
	// itself is pinned in cache.duplicate-ids.test.ts, because after a
	// verified load this page serves its in-memory snapshot first.
	const pageErrors = watchPageErrors(page);
	await page.addInitScript(
		({ key, contextKey, snapshot }) => {
			const context = 'duplicate-snapshot-account';
			window.sessionStorage.setItem(contextKey, context);
			window.sessionStorage.setItem(`${key}.${context}`, JSON.stringify(snapshot));
		},
		{
			key: SNAPSHOT_KEY,
			contextKey: AUTH_CONTEXT_KEY,
			snapshot: { savedAt: '2026-10-15T11:00:00.000Z', items: DUPLICATE_ROWS }
		}
	);
	await page.route('**/api/library', (route) => route.abort('failed'));

	await page.goto('/library');

	await expect(page.getByRole('alert')).toContainText(/not reachable/i);
	await expect(page.getByTestId('library-list')).toHaveCount(0);
	await expect(page.getByTestId('library-stale-badge')).toHaveCount(0);
	expect(pageErrors).toEqual([]);
});
