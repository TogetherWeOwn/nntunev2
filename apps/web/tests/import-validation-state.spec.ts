import { expect, test } from '@playwright/test';

// Builder-owned state-transition coverage alongside the protected smoke tests.
test('import feedback resets on reselection and clearing without making requests', async ({ page }) => {
	await page.goto('/library/import');
	await page.waitForLoadState('networkidle');
	const requests: string[] = [];
	page.on('request', (request) => requests.push(`${request.method()} ${request.url()}`));
	const input = page.getByTestId('import-file');
	const feedback = page.getByTestId('import-status');

	await input.setInputFiles({
		name: 'empty.nzb', mimeType: 'application/x-nzb', buffer: Buffer.alloc(0)
	});
	await expect(feedback).toHaveAttribute('role', 'alert');
	await expect(feedback).toContainText('empty');
	await expect(input).toHaveAttribute('aria-describedby', 'import-requirements import-feedback');

	// The inclusive cap and case-insensitive extension are reachable in the UI.
	await input.setInputFiles({
		name: 'ALBUM.NZB',
		mimeType: 'application/octet-stream',
		buffer: Buffer.alloc(2 * 1024 * 1024)
	});
	await expect(feedback).toHaveAttribute('role', 'status');
	await expect(feedback).toContainText('not available yet');
	await expect(page.getByRole('alert')).toHaveCount(0);

	await input.setInputFiles({
		name: 'notes.txt', mimeType: 'text/plain', buffer: Buffer.from('notes')
	});
	await expect(feedback).toHaveAttribute('role', 'alert');
	await expect(feedback).toContainText('.nzb');
	await expect(page.getByRole('status')).toHaveCount(0);

	await input.setInputFiles([]);
	await expect(feedback).toHaveCount(0);
	await expect(input).toHaveAttribute('aria-describedby', 'import-requirements');
	expect(requests).toEqual([]);
});
