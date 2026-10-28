import { expect, test } from '@playwright/test';
import { previewSourceMetadata } from './nzb-preview-evidence.mjs';
import { platform, arch, release } from 'node:os';
import playwrightPackage from '@playwright/test/package.json' with { type: 'json' };

// Fixed independently of measurements. The existing CI Playwright container
// includes all three engines; missing local engines are not a passing result.
const shapes: { id: string; build: () => string; guarded?: boolean; error?: string }[] = [
	{ id: 'tiny-elements-original', build: () => `<nzb>${'<m/>'.repeat(524000)}</nzb>`, guarded: true },
	{ id: 'comment-text-original', build: () => `<nzb>${'x<!---->'.repeat(260000)}</nzb>`, guarded: true },
	{ id: 'deep-markup-original', build: () => `<nzb>${'<m>'.repeat(260000)}${'</m>'.repeat(260000)}</nzb>`, guarded: true },
	{ id: 'boundary-150000', build: () => `<nzb>${'<m/>'.repeat(149998)}</nzb>` },
	{ id: 'boundary-150001', build: () => `<nzb>${'<m/>'.repeat(149999)}</nzb>`, guarded: true },
	{ id: 'paired-boundary', build: () => `<nzb>${'<m></m>'.repeat(74999)}</nzb>` },
	{ id: 'comment-text-boundary', build: () => `<nzb>${'x<!---->'.repeat(149998)}</nzb>` },
	{ id: 'comment-literal-false-reject', build: () => `<nzb><!--${'<'.repeat(149998)}--></nzb>`, guarded: true },
	{ id: 'cdata-literal-false-reject', build: () => `<nzb><![CDATA[${'<'.repeat(149998)}]]></nzb>`, guarded: true },
	{ id: 'doctype-pre-excision', build: () => `<!DOCTYPE nzb><nzb>${'<m/>'.repeat(149998)}</nzb>`, guarded: true },
	{ id: 'deep-nesting', build: () => `<nzb>${'<wrap>'.repeat(63)}<leaf/>${'</wrap>'.repeat(63)}</nzb>`, error: 'NZBP_TOO_DEEP' },
	{ id: 'element-comment-density', build: () => `<nzb>${'<m/>'.repeat(60000)}${'<!-- density-comment -->'.repeat(40000)}</nzb>` },
	{ id: 'near-cap-attribute', build: () => `<nzb><meta note="${'a'.repeat(1900000)}"/></nzb>`, error: 'NZBP_TOO_MANY_NODES' },
	{ id: 'near-cap-text', build: () => `<nzb><file><segments><segment bytes="1">${'t'.repeat(1900000)}</segment></segments></file></nzb>`, error: 'NZBP_TOO_MANY_NODES' },
	{ id: 'reference-dense', build: () => `<nzb><segment bytes="1">${'&#65;'.repeat(100000)}</segment></nzb>`, error: 'NZBP_TOO_MANY_NODES' },
	{ id: 'sixty-k-segments', build: () => `<nzb><file><segments>${'<segment bytes="1">x</segment>'.repeat(60000)}</segments></file></nzb>` }
];

for (const engine of ['chromium', 'firefox', 'webkit'] as const) {
	test.describe(`NZBP hostile budgets ${engine}`, () => {
		for (const shape of shapes) {
			test(`NZBP-R2-${shape.id} five individual fixed-budget repetitions`, async ({ playwright, baseURL }, info) => {
				expect(info.config.workers, 'Hostile shapes must not overlap other tests').toBe(1);
				const browser = await playwright[engine].launch();
				try {
				const page = await browser.newPage({ baseURL, serviceWorkers: 'block' });
				await page.addInitScript(() => {
					const parse = DOMParser.prototype.parseFromString;
					DOMParser.prototype.parseFromString = function (...args) {
						const start = performance.now();
						try { return parse.apply(this, args); }
						finally {
							const w = window as Window & { __nzbpParserTimes?: number[] };
							(w.__nzbpParserTimes ??= []).push(performance.now() - start);
						}
					};
				});
				await page.goto('/library/import');
				const xml = shape.build();
				const inputBytes = Buffer.byteLength(xml);
				expect(inputBytes).toBeLessThanOrEqual(2097152);
				const reps: { parse_ms: number | null; parser_invoked: boolean; parse_count: number; total_ms: number; outcome: unknown; verdict: string }[] = [];
				for (let rep = 0; rep < 5; rep++) {
					await page.getByTestId('import-file').setInputFiles({ name: 'hostile.nzb', mimeType: '', buffer: Buffer.from(xml) });
					await page.evaluate(() => { (window as Window & { __nzbpParserTimes?: number[] }).__nzbpParserTimes = []; });
					const start = Date.now();
					await page.getByRole('button', { name: 'Preview', exact: true }).click();
					await page.waitForFunction(() => !!(window as Window & { __nzbpLastOutcome?: unknown }).__nzbpLastOutcome);
					const totalMs = Date.now() - start;
					const actual = await page.evaluate(() => {
						const w = window as Window & { __nzbpParserTimes: number[]; __nzbpLastOutcome: { status: string; error_code: string | null; next_action: string } };
						const { status, error_code, next_action } = w.__nzbpLastOutcome;
						return { times: w.__nzbpParserTimes, outcome: { status, error_code, next_action } };
					});
					const expectedCode = shape.guarded ? 'NZBP_TOO_MANY_NODES' : shape.error ?? null;
					const correct = actual.outcome.status === (expectedCode ? 'error' : 'ok') && actual.outcome.error_code === expectedCode &&
						actual.outcome.next_action === (expectedCode ? 'choose_different_file' : 'none');
					const passed = correct && totalMs <= 1000 && (shape.guarded
						? actual.times.length === 0
						: actual.times.length === 1 && actual.times[0]! <= 500);
					reps.push({ parse_ms: actual.times.length === 1 ? actual.times[0]! : null, parser_invoked: actual.times.length > 0,
						parse_count: actual.times.length, total_ms: totalMs, outcome: actual.outcome, verdict: passed ? 'PASS' : 'FAIL' });
				}
				const evidence = {
					case_id: `NZBP-R2-${shape.id}`, engine, browser_version: browser.version(), playwright_version: playwrightPackage.version,
					...previewSourceMetadata(),
					runner: process.env['GITHUB_ACTIONS'] ? 'github-hosted-playwright-container' : 'local-test',
					os: `${platform()} ${arch()} ${release()}`, actual_bytes: inputBytes, parse_budget_ms: 500, total_budget_ms: 1000, reps
				};
				console.log(JSON.stringify(evidence));
				await info.attach('fixed-budget-evidence', { contentType: 'application/json', body: JSON.stringify(evidence) });
				expect(reps.every((rep) => rep.verdict === 'PASS'), JSON.stringify(evidence)).toBe(true);
				} finally { await browser.close(); }
			});
		}
	});
}
