import { expect, test, type Page } from '@playwright/test';

const sessionA = 'A'.repeat(43);
const sessionB = 'E'.repeat(43);
const catalogue = [
	{ id: 'first', title: 'First Song' },
	{ id: 'second', title: 'Second Song' },
	{ id: 'third', title: 'Third Song' }
];
const queueRows = (page: Page) => page.getByTestId('queue-item');
const currentRow = (page: Page) => page.locator('[data-testid="queue-item"][aria-current="true"]');
const listings = (page: Page) => page.getByTestId('library-item');
const libraryRow = (page: Page, id: string) => page.locator(`[data-testid="library-item"][data-id="${id}"] button.nt-library-row`);
const deferred = () => Promise.withResolvers<void>();

async function mockLibrary(page: Page) {
	let binding = sessionA;
	await page.route('**/v1/me/likes/session', (route) => route.fulfill({ json: { session_binding: binding } }));
	await page.route('**/api/library', (route) => route.fulfill({
		headers: { 'x-nntune-session-binding': binding }, json: catalogue
	}));
	return { replaceAccount() { binding = sessionB; } };
}

async function addTracks(page: Page) {
	await page.goto('/library');
	await expect(listings(page)).toHaveCount(3);
	for (const { id } of catalogue) await libraryRow(page, id).click();
	await expect(queueRows(page)).toHaveCount(3);
}

test('Library restores arranged order and selection only after authorization', async ({ page }) => {
	const playback: string[] = [];
	await page.route(/\/v1\/.*playback|\/api\/demo\/audio/, (route) => {
		playback.push(new URL(route.request().url()).pathname);
		return route.abort();
	});
	await mockLibrary(page);
	await addTracks(page);
	await page.getByRole('button', { name: 'Move Second Song up', exact: true }).click();
	await page.getByRole('button', { name: 'Select Third Song', exact: true }).click();
	await page.getByRole('button', { name: 'Remove First Song from queue', exact: true }).click();
	await expect(queueRows(page)).toHaveText([/Second Song/, /Third Song/]);
	await expect(currentRow(page)).toHaveAttribute('data-track-id', 'third');

	const verifying = deferred(), release = deferred();
	await page.route('**/v1/me/likes/session', async (route) => {
		verifying.resolve();
		await release.promise;
		await route.fulfill({ json: { session_binding: sessionA } });
	}, { times: 1 });
	await page.reload();
	await verifying.promise;
	try {
		await expect(queueRows(page)).toHaveCount(0);
	} finally {
		release.resolve();
	}
	await expect(listings(page)).toHaveCount(3);
	await expect(queueRows(page)).toHaveText([/Second Song/, /Third Song/]);
	await expect(currentRow(page)).toHaveAttribute('data-track-id', 'third');
	await expect(page.locator('audio[src], video[src], source[src]')).toHaveCount(0);
	expect(playback).toEqual([]);

	await page.getByRole('button', { name: 'Clear queue', exact: true }).click();
	await page.getByRole('button', { name: 'Yes, clear queue', exact: true }).click();
	await page.reload();
	await expect(listings(page)).toHaveCount(3);
	await expect(queueRows(page)).toHaveCount(0);
});
