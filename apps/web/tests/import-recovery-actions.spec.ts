import { expect, test, type Locator } from '@playwright/test';

// Builder-owned coverage for native same-file reselection and focus preservation.
test.use({ serviceWorkers: 'block' });

test.beforeEach(async ({ page, context, baseURL }) => {
	const origin = new URL(baseURL!).origin;
	expect(['localhost', '127.0.0.1', '[::1]']).toContain(new URL(origin).hostname);
	await context.route('**/*', (route) => new URL(route.request().url()).origin === origin
		&& ['GET', 'HEAD', 'OPTIONS'].includes(route.request().method())
		? route.continue() : route.abort('blockedbyclient'));
	await page.goto('/library/import');
	await page.waitForLoadState('networkidle');
});

async function observeFeedback(feedback: Locator) {
	await feedback.evaluate((element) => {
		const commits: string[] = [];
		Object.defineProperty(element, '__commits', { value: commits, configurable: true });
		new MutationObserver(() => commits.push(element.textContent?.trim() ?? ''))
			.observe(element, { subtree: true, childList: true, characterData: true });
	});
}

async function nonEmptyCommits(feedback: Locator) {
	return feedback.evaluate((element) => (element as Element & { __commits: string[] })
		.__commits.filter((text) => text.length > 0));
}

for (const name of ['same.nzb', 'same.txt']) {
	test(`native same-file cancel path refreshes ${name} feedback once without stealing focus`, async ({ page }) => {
		const requests: string[] = [];
		page.on('request', (request) => requests.push(`${request.method()} ${request.url()}`));
		const input = page.getByTestId('import-file');
		const feedback = page.getByTestId('import-status');
		await input.focus();
		await input.setInputFiles({ name, mimeType: 'application/octet-stream', buffer: Buffer.from('fixture') });
		await expect(feedback).toHaveAttribute('role', name.endsWith('.txt') ? 'alert' : 'status');
		const text = (await feedback.textContent())!.trim();
		await observeFeedback(feedback);
		for (let action = 1; action <= 2; action += 1) {
			// Browsers fire cancel (rather than change) for the same file again.
			await input.dispatchEvent('cancel');
			await expect.poll(() => nonEmptyCommits(feedback)).toEqual(Array(action).fill(text));
			await expect(input).toBeFocused();
		}
		// Hover, Escape and blur are not selection actions and must stay silent.
		await page.getByTestId('import-drop-area').dispatchEvent('dragover');
		await page.keyboard.press('Escape');
		await page.evaluate(() => window.dispatchEvent(new Event('blur')));
		await page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
		expect(await nonEmptyCommits(feedback)).toEqual([text, text]);
		expect(requests).toEqual([]);
	});
}

test('replacement keeps a focused Clear control mounted across valid and invalid outcomes', async ({ page }) => {
	const input = page.getByTestId('import-file');
	const clear = page.getByRole('button', { name: 'Clear selection', exact: true });
	await input.setInputFiles({ name: 'valid.nzb', mimeType: 'application/octet-stream', buffer: Buffer.from('fixture') });
	await clear.focus();
	for (const name of ['invalid.txt', 'replacement.nzb']) {
		await input.setInputFiles({ name, mimeType: 'application/octet-stream', buffer: Buffer.from('fixture') });
		await expect(page.getByTestId('import-status')).toHaveAttribute('role', name.endsWith('.txt') ? 'alert' : 'status');
		await expect(clear).toBeFocused();
	}
	await page.keyboard.press('Space');
	await expect(input).toBeFocused();
	await expect(page.getByTestId('import-status')).toHaveCount(0);
	await expect(page.locator('main [aria-live="polite"]')).toHaveText('Local selection cleared.');
});

for (const colorScheme of ['light', 'dark'] as const) {
	test(`${colorScheme} replacement removes rejection styling at mobile width`, async ({ page }) => {
		await page.setViewportSize({ width: 390, height: 844 });
		await page.emulateMedia({ colorScheme, reducedMotion: 'reduce' });
		const input = page.getByTestId('import-file');
		const feedback = page.getByTestId('import-status');
		const normalColor = await page.getByRole('main').evaluate((element) => getComputedStyle(element).color);
		for (let action = 0; action < 10; action += 1) {
			await input.setInputFiles({ name: 'bad.txt', mimeType: 'text/plain', buffer: Buffer.from('fixture') });
			await expect(feedback).toHaveAttribute('role', 'alert');
			await expect(feedback).not.toHaveCSS('color', normalColor);
			await input.setInputFiles({ name: 'good.nzb', mimeType: 'application/octet-stream', buffer: Buffer.from('fixture') });
			await expect(feedback).toHaveAttribute('role', 'status');
			await expect(feedback).toHaveCSS('color', normalColor);
		}
	});
}
