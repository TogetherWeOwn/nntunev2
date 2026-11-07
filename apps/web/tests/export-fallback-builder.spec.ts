import { expect, test } from '@playwright/test';
import { openQueue } from './helpers/queue-host';

const title = '<b>Plain text</b>';
const text = `1. ${title} — Artist`;

for (const surface of ['demo', 'queue', 'favorites', 'selection'] as const) {
	test(`${surface} export preserves its labeled selectable manual-copy field`, async ({ page }) => {
		await page.addInitScript(() => {
			Object.defineProperty(navigator, 'clipboard', { value: undefined, configurable: true });
		});
		let prefix = `${surface}-export`;
		let label: string;
		let expectedText = text;
		if (surface === 'demo') {
			prefix = 'queue-export';
			label = 'Queue as text';
			await page.goto('/demo/player');
			expectedText = '1. Demo track — Fixture Band\n2. Second Signal — Fixture Band\n3. Third Light — Session Players';
		} else {
			await page.route('**/api/library', (route) => route.fulfill({
				json: [{ id: 'fallback-track', title, artist: 'Artist' }]
			}));
			await page.goto('/library');
			const row = page.getByTestId('library-item');
			await expect(row).toHaveCount(1);
			if (surface === 'selection') {
				label = 'Selected tracks as text';
				await row.getByRole('checkbox').check();
			} else if (surface === 'favorites') {
				label = 'Favorites as text';
				await row.getByRole('button', { name: /^Star / }).click();
			} else {
				label = 'Queue as text';
				await page.getByRole('button', { name: 'Add displayed tracks to queue', exact: true }).click();
				await openQueue(page);
			}
		}
		const copy = surface === 'queue'
			? page.getByTestId('queue-panel').getByRole('button', { name: 'Copy queue as text', exact: true })
			: page.getByTestId(`${prefix}-copy`);
		await copy.focus();
		await copy.click();
		const field = page.getByRole('textbox', { name: label, exact: true });
		await expect(field).toHaveAttribute('data-testid', `${prefix}-fallback`);
		await expect(field).toHaveAttribute('rows', surface === 'selection' ? '5' : '4');
		await expect(field).toHaveJSProperty('readOnly', true);
		await expect(field).toHaveValue(expectedText);
		await expect(field).toHaveCSS('display', 'block');
		await expect(copy).toBeFocused();
		const instructions = page.getByTestId(`${prefix}-instructions`);
		await expect(instructions).toBeVisible();
		await expect(instructions).toContainText(/copy/i);
		await expect(field).toHaveAttribute('aria-describedby', await instructions.getAttribute('id') ?? 'missing-instructions');
		await field.focus();
		const selection = await field.evaluate((node: HTMLTextAreaElement) => {
			node.select();
			return { start: node.selectionStart, end: node.selectionEnd, children: node.children.length };
		});
		expect(selection).toEqual({ start: 0, end: expectedText.length, children: 0 });
	});
}
