import { expect, test } from '@playwright/test';

// Supplements the acceptance bank with near-2-MiB shapes (~500k parser nodes),
// not just near-cap text/attributes. Fixed budgets, never derived from results.
const shapes = {
	'tiny-elements': () => `<nzb>${'<m/>'.repeat(524000)}</nzb>`,
	'comment-text-density': () => `<nzb>${'x<!---->'.repeat(260000)}</nzb>`,
	'deep-markup': () => `<nzb>${'<m>'.repeat(260000)}${'</m>'.repeat(260000)}</nzb>`
};

for (const [caseId, build] of Object.entries(shapes)) {
	test(`NZBP-R2-extra-${caseId} stays inside fixed parser and total budgets`, async ({ page, browserName }, info) => {
		await page.addInitScript(() => {
			const parse = DOMParser.prototype.parseFromString;
			DOMParser.prototype.parseFromString = function (...args) {
				const start = performance.now();
				try { return parse.apply(this, args); }
				finally { (window as Window & { __nzbpParserMs?: number }).__nzbpParserMs = performance.now() - start; }
			};
		});
		await page.goto('/library/import');
		const xml = build();
		expect(Buffer.byteLength(xml)).toBeLessThanOrEqual(2097152);
		const reps: { parse_ms: number; total_ms: number }[] = [];
		for (let rep = 0; rep < 5; rep++) {
			await page.getByTestId('import-file').setInputFiles({ name: 'hostile.nzb', mimeType: '', buffer: Buffer.from(xml) });
			const start = Date.now();
			await page.getByRole('button', { name: 'Preview', exact: true }).click();
			await page.waitForFunction(() => !!(window as Window & { __nzbpLastOutcome?: unknown }).__nzbpLastOutcome);
			reps.push({
				parse_ms: await page.evaluate(() => (window as Window & { __nzbpParserMs: number }).__nzbpParserMs),
				total_ms: Date.now() - start
			});
		}
		const evidence = { case_id: `NZBP-R2-extra-${caseId}`, engine: browserName, input_bytes: Buffer.byteLength(xml), parse_budget_ms: 500, total_budget_ms: 1000, reps };
		console.log(JSON.stringify(evidence));
		await info.attach('fixed-budget-evidence', { contentType: 'application/json', body: JSON.stringify(evidence) });
		expect(reps.every((rep) => rep.parse_ms <= 500 && rep.total_ms <= 1000), JSON.stringify(evidence)).toBe(true);
	});
}
