import { expect, test } from '@playwright/test';
import { openQueue } from './helpers/queue-host';

/*
 * Narrow exception: the Engineering Lead authorized a one-time migration, in
 * the same pull request, of this file's physical queue host, opening, locator
 * and lifecycle setup to the persistent shell's Queue panel. Every assertion
 * and behavior vector remains protected, and an independent equivalence
 * review is required.
 */

// Builder-owned companion to BULK-03. Keep the protected acceptance bank intact.
// Catches a queue row losing the leading separator space after a bulk append.
test('bulk-added queue rows match the now-playing title and artist spacing', async ({ page, context }) => {
	await context.route('**/*', (route) => {
		const url = new URL(route.request().url());
		if (!['localhost', '127.0.0.1'].includes(url.hostname)) return route.abort('blockedbyclient');
		if (url.pathname.startsWith('/api/')) return route.fulfill({ status: 404, contentType: 'application/json', body: '{}' });
		return route.continue();
	});
	await page.route('**/api/library', (route) => route.fulfill({
		contentType: 'application/json',
		body: JSON.stringify([
			{ id: 'queue-z', title: 'Zulu Song', artist: '' },
			{ id: 'queue-m', title: 'Middle Song', artist: 'Alpha Band' },
			{ id: 'queue-untitled', title: '  ', artist: 'Bravo Band' }
		])
	}));
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(3);
	await openQueue(page);
	await page.getByRole('combobox', { name: /order|sort/i }).selectOption({ label: 'Artist A–Z' });
	await page.getByRole('button', { name: 'Add displayed tracks to queue', exact: true }).click();
	const rows = page.getByTestId('queue-item');
	await expect(rows).toHaveCount(3);
	await expect(rows.nth(0)).toHaveAttribute('data-track-id', 'queue-m');
	await expect(rows.nth(0)).toContainText('Middle Song — Alpha Band');
	await expect(page.getByTestId('now-playing')).toContainText('Middle Song — Alpha Band');
	await expect(rows.nth(1)).toContainText('queue-untitled — Bravo Band');
	await expect(rows.nth(2)).toContainText('Zulu Song');
	await expect(rows.nth(2)).not.toContainText('—');
});
