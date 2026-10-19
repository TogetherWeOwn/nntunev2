import { expect, test as base } from '@playwright/test';

const test = base.extend<{ localTraffic: void }>({
	localTraffic: [
		async ({ context, baseURL }, use, testInfo) => {
			const origin = new URL(baseURL!);
			expect(['localhost', '127.0.0.1', '[::1]']).toContain(origin.hostname);
			const mutations: string[] = [];
			const external: string[] = [];
			const requests: string[] = [];
			const isRead = (method: string) => ['GET', 'HEAD', 'OPTIONS'].includes(method);

			// Install before navigation. Observe attempted requests even when blocked;
			// abort every mutation and all off-origin traffic, including provider GETs.
			context.on('request', (request) => {
				const entry = `${request.method()} ${request.url()}`;
				requests.push(entry);
				if (!isRead(request.method())) mutations.push(entry);
				if (new URL(request.url()).origin !== origin.origin) external.push(entry);
			});
			await context.route('**/*', (route) => {
				const request = route.request();
				return !isRead(request.method()) || new URL(request.url()).origin !== origin.origin
					? route.abort('blockedbyclient')
					: route.continue();
			});

			await use();

			await testInfo.attach('local-selection-network', {
				body: JSON.stringify({ mutations, external, requests }, null, 2),
				contentType: 'application/json'
			});
			expect.soft(mutations, `Unexpected mutation requests: ${mutations.join(', ')}`).toEqual([]);
			expect.soft(external, `Unexpected external requests: ${external.join(', ')}`).toEqual([]);
		},
		{ auto: true }
	]
});

// Service workers must not bypass interception. No real NZB, credentials,
// ingest endpoint, staging service, or provider is used by these tests.
test.use({ serviceWorkers: 'block' });

const syntheticNzb = {
	name: 'local-selection.nzb',
	mimeType: 'application/x-nzb',
	buffer: Buffer.from('<?xml version="1.0"?><nzb></nzb>')
};

function expectLocalSelection(text: string): void {
	expect.soft(text, 'Describe selection, not an import result').toMatch(
		/\b(?:select(?:s|ed|ing|ion)?|choos(?:e|ing)|chosen|pick(?:ed|ing)?)\b/i
	);
	expect.soft(text, 'Explain that the selection stays local').toMatch(/\blocal(?:ly)?\b/i);
	// Reject a promise of a future mutation, but allow negated disclosure copy.
	expect.soft(text, 'Do not promise upload, processing, or library import').not.toMatch(
		/\b(?:to|will|can)\s+(?:add|import|upload|process)\b/i
	);
}

function expectNoImportEffects(text: string): void {
	// Allow shared negation: "does not upload or process ... or add tracks".
	// Do not carry it across sentences, semicolons, or contrasting clauses.
	const clauses = text.replace(/\b(?:but|however)\b/gi, ';');
	const negation = String.raw`\b(?:no|not|never|without|doesn['’]t|isn['’]t|won['’]t)\b[^.!?;]*`;
	expect.soft(clauses, 'Explicitly disclose no upload').toMatch(
		new RegExp(`${negation}\\bupload(?:s|ed|ing)?\\b`, 'i')
	);
	expect.soft(clauses, 'Explicitly disclose no processing').toMatch(
		new RegExp(`${negation}\\bprocess(?:es|ed|ing)?\\b`, 'i')
	);
	const tracks = String.raw`\btracks?\b[^.!?;]*`;
	const added = String.raw`\badd(?:s|ed|ing)?\b`;
	expect.soft(clauses, 'Explicitly disclose no tracks added').toMatch(
		new RegExp(`${negation}(?:${added}[^.!?;]*${tracks}|${tracks}${added})|${tracks}${negation}${added}`, 'i')
	);
	expect.soft(text, 'A local selection is not a received file').not.toMatch(/\bfile\s+received\b/i);
}

test('NZB selection discloses local-only behavior before choosing a file', async ({ page }) => {
	await page.goto('/library/import');
	const input = page.getByLabel('NZB file', { exact: true });
	await expect(input).toBeVisible();
	await expect(input).toHaveAttribute('data-testid', 'import-file');
	await expect(input).toHaveAttribute('type', 'file');
	await expect(input).toHaveAttribute('accept', '.nzb');
	await expect(input).toHaveJSProperty('value', '');
	await expect(page.getByTestId('import-status')).toHaveCount(0);

	const copy = await page.getByRole('main').innerText();
	expectLocalSelection(copy);
	expect.soft(copy, 'Make the local-only limitation clear before selection').toMatch(/\b(?:only|just)\b/i);
	expectNoImportEffects(copy);

	const description = await page.locator('meta[name="description"]').getAttribute('content');
	expectLocalSelection(description ?? '');
});

test('NZB selection status says selected locally, not received or imported', async ({ page }) => {
	await page.goto('/library/import');
	await page.getByTestId('import-file').setInputFiles(syntheticNzb);
	const status = page.getByTestId('import-status');
	await expect(status).toBeVisible();
	await expect(status).toHaveAttribute('role', 'status');
	const copy = await status.innerText();
	expectLocalSelection(copy);
	expect.soft(copy, 'Report a selection, not just instructions to select').toMatch(/\bselected\b/i);
	expectNoImportEffects(copy);
});

test('NZB empty selection and clearing remove status without sending mutations', async ({ page }) => {
	await page.goto('/library/import');
	const input = page.getByTestId('import-file');
	const status = page.getByTestId('import-status');
	await expect(input).toBeVisible();
	await expect(status).toHaveCount(0);

	await input.setInputFiles([]);
	await expect(input).toHaveJSProperty('value', '');
	await expect(status).toHaveCount(0);

	await input.setInputFiles(syntheticNzb);
	expect(await input.evaluate((element: HTMLInputElement) => element.files?.[0]?.name)).toBe(
		syntheticNzb.name
	);
	await expect(status).toBeVisible();
	await expect(status).toHaveAttribute('role', 'status');

	await input.setInputFiles([]);
	await expect(input).toHaveJSProperty('value', '');
	await expect(status).toHaveCount(0);
	await expect(page.getByRole('status')).toHaveCount(0);
	// Native cancel may retain the current FileList; only an actual empty
	// change is a reset, so no synthetic cancel behavior is imposed here.
});
