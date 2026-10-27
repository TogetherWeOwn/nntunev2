import { expect, test } from '@playwright/test';

// Builder-owned supplements: do not alter the independent acceptance bank.
const xml = '<nzb><file><segments><segment bytes="7" number="1">sentinel-private-id</segment></segments></file></nzb>';
const probe = (page: import('@playwright/test').Page) => page.evaluate(() =>
	(window as Window & { __nzbpLastOutcome?: unknown }).__nzbpLastOutcome
);

for (const malformed of [false, true]) {
	for (const action of ['cancel', 'clear', 'replace', 'invalid', 'chooser'] as const) {
		test(`queued ${action} inside ${malformed ? 'malformed' : 'valid'} parse discards before traversal`, async ({ page }) => {
			await page.addInitScript(({ action }) => {
				const original = DOMParser.prototype.parseFromString;
				DOMParser.prototype.parseFromString = function (...args) {
					const parsed = original.apply(this, args);
					(window as Window & { __nzbpTraversals?: number }).__nzbpTraversals = 0;
					const root = parsed.documentElement;
					Object.defineProperty(parsed, 'documentElement', { get() {
						const w = window as Window & { __nzbpTraversals: number };
						w.__nzbpTraversals++;
						return root;
					} });
					setTimeout(() => {
						const input = document.querySelector('[data-testid="import-file"]') as HTMLInputElement;
						if (action === 'cancel' || action === 'clear') {
							Array.from(document.querySelectorAll('button')).find((b) => b.textContent?.trim() === (action === 'cancel' ? 'Cancel' : 'Clear selection'))?.click();
						} else if (action === 'chooser') input.dispatchEvent(new Event('cancel', { bubbles: true }));
						else {
							const prior = input.files![0]!;
							const transfer = new DataTransfer();
							transfer.items.add(new File([' '.repeat(prior.size)], action === 'invalid' ? 'replacement.txt' : prior.name, { lastModified: prior.lastModified }));
							input.files = transfer.files;
							input.dispatchEvent(new Event('change', { bubbles: true }));
						}
					}, 0);
					return parsed;
				};
			}, { action });
			await page.goto('/library/import');
			await page.getByTestId('import-file').setInputFiles({ name: 'lifecycle.nzb', mimeType: '', buffer: Buffer.from(malformed ? '<nzb><broken></nzb>' : xml) });
			await page.getByRole('button', { name: 'Preview', exact: true }).click();
			await expect.poll(() => probe(page)).toEqual(action === 'cancel' ? { status: 'cancelled' } : null);
			await page.evaluate(() => new Promise((resolve) => setTimeout(resolve, 100)));
			expect(await probe(page)).toEqual(action === 'cancel' ? { status: 'cancelled' } : null);
			expect(await page.evaluate(() => (window as Window & { __nzbpTraversals?: number }).__nzbpTraversals)).toBe(0);
			await expect(page.getByRole('heading', { name: 'Local preview of what this NZB claims' })).toHaveCount(0);
			await expect(page.getByRole('main')).not.toContainText('sentinel-private-id');
		});
	}
}

for (const mode of ['overflow', 'cancel'] as const) {
	test(`arrayBuffer fallback ${mode} never publishes a decoded or stale result`, async ({ page }) => {
		await page.addInitScript(({ mode }) => {
			Object.defineProperty(Blob.prototype, 'stream', { value: undefined });
			Blob.prototype.arrayBuffer = function () {
				if (mode === 'overflow') return Promise.resolve(new ArrayBuffer(2 * 1024 * 1024 + 1));
				return new Promise((resolve) => {
					setTimeout(() => Array.from(document.querySelectorAll('button')).find((b) => b.textContent?.trim() === 'Cancel')?.click(), 0);
					setTimeout(() => resolve(new TextEncoder().encode('<nzb/>').buffer), 50);
				});
			};
		}, { mode });
		await page.goto('/library/import');
		await page.getByTestId('import-file').setInputFiles({ name: 'fallback.nzb', mimeType: '', buffer: Buffer.from(xml) });
		await page.getByRole('button', { name: 'Preview', exact: true }).click();
		await expect.poll(() => probe(page)).toEqual(mode === 'overflow'
			? { status: 'error', error_code: 'NZBP_TOO_LARGE', next_action: 'choose_different_file' }
			: { status: 'cancelled' });
		await page.evaluate(() => new Promise((resolve) => setTimeout(resolve, 100)));
		await expect(page.getByRole('heading', { name: 'Local preview of what this NZB claims' })).toHaveCount(0);
	});
}
