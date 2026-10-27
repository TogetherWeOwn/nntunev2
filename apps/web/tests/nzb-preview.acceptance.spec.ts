import { expect, test as base, type Page, type TestInfo } from '@playwright/test';

/**
 * NZBP — protected acceptance bank, browser half: bounded local NZB claims
 * preview on the existing import page. The fast half is
 * `src/lib/library/nzb-preview.acceptance.test.ts` (pure pre-scan/summary
 * core, caps, drift, source mechanisms). Read both headers: together they
 * are the complete build contract.
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build implements the
 * contract below on top of this branch and must make these tests pass
 * WITHOUT editing this file; a wrong test comes back to the acceptance
 * owner instead. Existing import/drop/echo/selection specs stay
 * authoritative for selection behaviour; this bank pins only the preview
 * gesture, its outcomes, and its safety properties.
 *
 * LOCKED PAGE CONTRACT (builder-owned, on `/library/import`):
 * - An explicit native Preview button (accessible name exactly `Preview`),
 *   always visible, disabled unless a valid single selection exists. While
 *   reading it is replaced in the same slot by Cancel (name `Cancel`).
 *   Selection alone never reads file contents. `Clear selection` keeps
 *   working and also drops any summary, error and probe value.
 * - After every settled preview the page exposes the exact outcome JSON on
 *   `window.__nzbpLastOutcome` (test seam; mirrors the rendered result and
 *   the fast bank's `NzbpOutcome` shape): the ok/error object, or
 *   `{ status: 'cancelled' }` after a user cancel. It is `null` after any
 *   selection change or Clear, and stale results never touch it. It must
 *   never contain file-content strings.
 * - Read path: chunked `stream()` with actual-byte accounting (a `file.size`
 *   pre-check alone is a time-of-check hole), fatal UTF-8 decode, single
 *   built-in `DOMParser` step, at least one task-turn yield with a
 *   generation/identity re-check before anything is published. Every
 *   selection mutation bumps the generation first; `arrayBuffer()` fallback
 *   is non-interruptible and generation-discarded only.
 * - Copy: the exact heading, counts, totals, warnings, always-present line
 *   and disclaimer below, plus the readable-preview-does-not-mean-intake
 *   sentence. Forbidden words (`verified, available, ready, safe, complete,
 *   tracks, songs, your files`) appear only inside negated disclaimers;
 *   unknown totals never render as zero. Page/meta copy separates
 *   metadata-only selection from the explicit local read while keeping the
 *   import-unavailable statement truthful.
 * - Fixed budgets (measured by this bank, five repetitions per hostile
 *   shape per claimed engine): the synchronous parser step finishes within
 *   500 ms and the whole local preview within 1000 ms. A crash, hang,
 *   over-budget case or unverified claimed engine stops shipment. This
 *   bank runs on Chromium here; claiming another engine requires running
 *   this same spec on it and recording its five repetitions — the engine
 *   name is part of every timing receipt.
 *
 * Evidence: every case logs one compact JSON line
 * (`bank/case_id/engine/expected/actual/status/error_code/next_action`)
 * and attaches it. All fixtures are synthetic strings/buffers built below;
 * no real NZB, provider, credential, staging or production system is used.
 * Loopback only; service workers blocked; every mutation and every
 * off-origin request is aborted and recorded.
 *
 * Focused RED/GREEN (Chromium must already be installed; local hosts may
 * need the provisioned browser-library path; port 4173 must be free):
 *   CI=1 pnpm --dir apps/web test:e2e tests/nzb-preview.acceptance.spec.ts --workers=1
 */

type NzbpEvidence = {
	requests: string[];
	unsafeRequests: string[];
	consoleMessages: { type: string; text: string }[];
	pageErrors: string[];
};

const test = base.extend<{ nzbp: NzbpEvidence }>({
	nzbp: [
		async ({ context, page }, use) => {
			const evidence: NzbpEvidence = { requests: [], unsafeRequests: [], consoleMessages: [], pageErrors: [] };
			await context.addInitScript(() => {
				const w = window as unknown as { __nzbpReads: string[]; __nzbpParseMs: number[] };
				w.__nzbpReads = [];
				w.__nzbpParseMs = [];
				for (const [prototype, methods] of [
					[Blob.prototype, ['text', 'arrayBuffer', 'stream', 'slice', 'bytes']],
					[FileReader.prototype, ['readAsText', 'readAsArrayBuffer', 'readAsDataURL', 'readAsBinaryString']]
				] as const) {
					for (const method of methods) {
						const descriptor = Object.getOwnPropertyDescriptor(prototype, method);
						if (descriptor?.value) {
							Object.defineProperty(prototype, method, {
								...descriptor,
								value: function (this: unknown, ...args: unknown[]) {
									w.__nzbpReads.push(method);
									return (descriptor.value as (...inner: unknown[]) => unknown).apply(this, args);
								}
							});
						}
					}
				}
				const parseDescriptor = Object.getOwnPropertyDescriptor(DOMParser.prototype, 'parseFromString');
				if (parseDescriptor?.value) {
					Object.defineProperty(DOMParser.prototype, 'parseFromString', {
						...parseDescriptor,
						value: function (this: DOMParser, ...args: [string, DOMParserSupportedType]) {
							const started = performance.now();
							try {
								return (parseDescriptor.value as DOMParser['parseFromString']).apply(this, args);
							} finally {
								w.__nzbpParseMs.push(performance.now() - started);
							}
						}
					});
				}
			});
			context.on('request', (request) => {
				const entry = `${request.method()} ${request.url()}`;
				evidence.requests.push(entry);
				if (!['GET', 'HEAD', 'OPTIONS'].includes(request.method())) evidence.unsafeRequests.push(entry);
			});
			await context.route('**/*', (route) => {
				const request = route.request();
				return !['GET', 'HEAD', 'OPTIONS'].includes(request.method()) ? route.abort('blockedbyclient') : route.continue();
			});
			page.on('console', (message) => {
				evidence.consoleMessages.push({ type: message.type(), text: message.text().slice(0, 500) });
			});
			page.on('pageerror', (error) => {
				evidence.pageErrors.push(String(error).slice(0, 300));
			});
			await use(evidence);
		},
		{ auto: true }
	]
});

test.use({ serviceWorkers: 'block' });

// Copy deck: exact strings the build uses (OSS review may consciously
// supersede them, which revises this bank first — never silently).
const COPY = {
	heading: 'Local preview of what this NZB claims',
	alwaysLine: 'Local preview only — nothing was uploaded and nothing was checked on any server.',
	disclaimer:
		'A readable NZB does not mean the content exists, is safe, belongs to you, or will play. Names inside an NZB are written by whoever posted it and are not shown here.',
	intakeSentence: 'a readable preview does not mean a future import would accept this file',
	importUnavailable: 'import is not available yet'
} as const;

const USER_TEXT: Record<string, string> = {
	NZBP_EMPTY: 'This file is empty, so there is nothing to preview.',
	NZBP_TOO_LARGE: 'This file is larger than the 2 MiB preview limit.',
	NZBP_READ_FAILED: 'The file could not be read locally. Try preview again.',
	NZBP_ENCODING_UNSUPPORTED: 'This file is not UTF-8 XML 1.0, so it cannot be previewed.',
	NZBP_DOCTYPE_NOT_ALLOWED: 'This file declares XML entities, which previews never process.',
	NZBP_MALFORMED_XML: 'This file is not readable XML.',
	NZBP_NOT_NZB: 'This XML is not an NZB document.',
	NZBP_TOO_MANY_FILES: 'This file claims more than 5,000 file entries, over the preview limit.',
	NZBP_TOO_MANY_SEGMENTS: 'This file claims more than 60,000 segments, over the preview limit.',
	NZBP_TOO_DEEP: 'This file is nested too deeply to preview.',
	NZBP_TOO_MANY_NODES: 'This file has more structure than a preview allows.',
	NZBP_DECLARED_BYTES_OVER_CAP: 'This file declares more than 64 GiB, over the preview limit.',
	NZBP_PARSER_UNAVAILABLE: 'This browser cannot preview NZB files locally.'
};

const NEXT_ACTION: Record<string, string> = { NZBP_READ_FAILED: 'try_again', NZBP_PARSER_UNAVAILABLE: 'none' };
const nextActionFor = (code: string) => NEXT_ACTION[code] ?? 'choose_different_file';
const errOutcome = (code: string) => ({ status: 'error', error_code: code, next_action: nextActionFor(code) });

const SENTINELS = [
	'NZBP-SENTINEL-SUBJECT-7f3a',
	'NZBP-SENTINEL-POSTER-9c1e',
	'NZBP-SENTINEL-GROUP-4b77',
	'NZBP-SENTINEL-MID-2d8f',
	'NZBP-SENTINEL-COMMENT-6a01',
	'NZBP-SENTINEL-CDATA-11bc',
	'NZBP-SENTINEL-PI-83e4',
	'NZBP-SENTINEL-DTD-55f0'
];

const FORBIDDEN = [/\bverified\b/i, /\bavailable\b/i, /\bready\b/i, /\bsafe\b/i, /\bcomplete\b/i, /\btracks?\b/i, /\bsongs?\b/i, /\byour files\b/i];

function summary(overrides: Record<string, unknown> = {}): Record<string, unknown> {
	return {
		preview_config_version: 1,
		file_entry_count: 0,
		segment_count: 0,
		declared_bytes_total: null,
		declared_bytes_basis: 'no_segments',
		segments_without_byte_claim: 0,
		segments_without_number: 0,
		duplicate_segment_numbers: 0,
		duplicate_message_ids: 0,
		files_without_segments: 0,
		files_with_multiple_segment_groups: 0,
		...overrides
	};
}

function segmentXml(options: { bytes?: string; number?: string; text?: string; extra?: string }): string {
	const bytes = options.bytes === undefined ? '' : ` bytes="${options.bytes}"`;
	const number = options.number === undefined ? '' : ` number="${options.number}"`;
	return `<segment${bytes}${number}${options.extra ?? ''}>${options.text ?? 'mid'}</segment>`;
}

function fileXml(segments: string[], subject = 'entry'): string {
	return `<file poster="poster" date="1700000000" subject="${subject}"><groups><group>group</group></groups><segments>${segments}</segments></file>`;
}

function nzbXml(body: string, options: { declaration?: string; prolog?: string } = {}): string {
	const declaration = options.declaration ?? '<?xml version="1.0"?>';
	return `${declaration}${options.prolog ?? ''}<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">${body}</nzb>`;
}

async function openImport(page: Page, baseURL: string | undefined): Promise<void> {
	const origin = new URL(baseURL!);
	expect(['localhost', '127.0.0.1', '[::1]']).toContain(origin.hostname);
	await page.goto('/library/import');
	await expect(page.getByTestId('import-file')).toBeVisible();
}

async function setFileWithMeta(page: Page, name: string, bytes: number[], lastModified: number): Promise<void> {
	await page.evaluate(
		({ fileName, content, stamp }) => {
			const input = document.querySelector('[data-testid="import-file"]') as HTMLInputElement;
			const transfer = new DataTransfer();
			transfer.items.add(new File([new Uint8Array(content)], fileName, { type: 'application/x-nzb', lastModified: stamp }));
			input.files = transfer.files;
			input.dispatchEvent(new Event('input', { bubbles: true }));
			input.dispatchEvent(new Event('change', { bubbles: true }));
		},
		{ fileName: name, content: bytes, stamp: lastModified }
	);
}

async function selectNzb(page: Page, name: string, content: string | Buffer, lastModified?: number): Promise<void> {
	const buffer = Buffer.isBuffer(content) ? content : Buffer.from(content);
	if (lastModified === undefined) {
		await page.getByTestId('import-file').setInputFiles({ name, mimeType: 'application/x-nzb', buffer });
	} else {
		await setFileWithMeta(page, name, [...buffer], lastModified);
	}
	const status = page.getByTestId('import-status');
	await expect(status, 'valid synthetic selection is acknowledged').toBeVisible();
	await expect(status).toHaveAttribute('role', 'status');
	await expect(status).toContainText(/selected locally/i);
}

const previewButton = (page: Page) => page.getByRole('button', { name: 'Preview', exact: true });
const cancelButton = (page: Page) => page.getByRole('button', { name: 'Cancel', exact: true });

async function requirePreview(page: Page, caseId: string) {
	const button = previewButton(page);
	await expect(button, `${caseId}: explicit Preview gesture is present`).toBeVisible({ timeout: 3000 });
	await expect(button, `${caseId}: Preview is enabled for a valid selection`).toBeEnabled({ timeout: 3000 });
	return button;
}

async function probe(page: Page): Promise<unknown> {
	return page.evaluate(() => (window as unknown as { __nzbpLastOutcome?: unknown }).__nzbpLastOutcome);
}

async function settled(page: Page, timeout = 10000): Promise<unknown> {
	await page.waitForFunction(
		() => {
			const value = (window as unknown as { __nzbpLastOutcome?: unknown }).__nzbpLastOutcome;
			return value !== null && value !== undefined;
		},
		null,
		{ timeout }
	);
	return probe(page);
}

async function parseDurations(page: Page): Promise<number[]> {
	return page.evaluate(() => (window as unknown as { __nzbpParseMs: number[] }).__nzbpParseMs.slice());
}

async function readCalls(page: Page): Promise<string[]> {
	return page.evaluate(() => (window as unknown as { __nzbpReads: string[] }).__nzbpReads.slice());
}

async function engineOf(page: Page): Promise<string> {
	return page.context().browser()?.browserType().name() ?? 'unknown';
}

async function storageSnapshot(page: Page): Promise<unknown> {
	return page.evaluate(async () => ({
		local: { ...localStorage },
		session: { ...sessionStorage },
		databases: (await indexedDB.databases())
			.map(({ name, version }) => ({ name, version }))
			.sort((a, b) => (a.name ?? '').localeCompare(b.name ?? ''))
	}));
}

async function surfaces(page: Page, evidence: NzbpEvidence): Promise<{ dom: string; title: string; aria: string; console: string; probe: string; feedback: string }> {
	return {
		dom: await page.getByRole('main').innerText(),
		title: await page.title(),
		aria: await page.getByRole('main').ariaSnapshot(),
		console: evidence.consoleMessages.map((entry) => entry.text).join('\n'),
		probe: JSON.stringify((await probe(page)) ?? null),
		feedback: (await page.getByTestId('import-status').allTextContents()).join('\n')
	};
}

function expectNoEcho(surface: Record<string, string>, markers: string[], caseId: string): void {
	for (const [name, text] of Object.entries(surface)) {
		for (const marker of markers) {
			expect.soft(text, `${caseId}: sentinel absent from ${name}`).not.toContain(marker);
		}
	}
}

async function receipt(
	info: TestInfo,
	engine: string,
	caseId: string,
	expected: unknown,
	observe: () => Promise<unknown>,
	failCode: string,
	fix: string
): Promise<void> {
	let actual: unknown = { error: 'observation failed before assertion' };
	let thrown: unknown = null;
	try {
		actual = await observe();
		expect(actual).toEqual(expected);
	} catch (error) {
		thrown = error;
		if (actual === undefined) actual = { error: error instanceof Error ? error.message : String(error) };
	} finally {
		const line = JSON.stringify({
			bank: 'playwright',
			engine,
			case_id: caseId,
			expected,
			actual,
			status: thrown === null ? 'PASS' : 'FAIL',
			error_code: thrown === null ? null : failCode,
			next_action: thrown === null ? 'none' : fix
		});
		console.log(line);
		await info.attach(`nzbp-${caseId}`, { body: line, contentType: 'application/json' });
	}
	if (thrown) throw thrown;
}

const FIX_PAGE = 'Implement the preview gesture and outcome contract; preserve this protected bank.';

async function expectCleanRun(page: Page, evidence: NzbpEvidence, caseId: string): Promise<void> {
	expect.soft(evidence.pageErrors, `${caseId}: no uncaught page errors`).toEqual([]);
	expect.soft(evidence.unsafeRequests, `${caseId}: no mutation requests`).toEqual([]);
	const parserErrors = await page.evaluate(() => document.getElementsByTagName('parsererror').length);
	expect.soft(parserErrors, `${caseId}: parser nodes never attach to the live document`).toBe(0);
}

test.describe('NZBP controls and guards', () => {
	test('NZBP-CTRL-10 selection acknowledgement still works', async ({ page, baseURL }, info) => {
		await openImport(page, baseURL);
		await selectNzb(page, 'ctrl-10.nzb', nzbXml(fileXml([segmentXml({ bytes: '10', number: '1', text: 'mid' })])));
		await receipt(info, await engineOf(page), 'NZBP-CTRL-10', { selected: true }, async () => ({ selected: true }), 'NZBP_HARNESS', 'Send this case back to the acceptance owner.');
	});

	test('NZBP-CTRL-11 selection sends no mutation requests', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		await selectNzb(page, 'ctrl-11.nzb', nzbXml(fileXml([segmentXml({ bytes: '10', number: '1', text: 'mid' })])));
		await receipt(info, await engineOf(page), 'NZBP-CTRL-11', { unsafe: [] }, async () => ({ unsafe: nzbp.unsafeRequests }), 'NZBP_HARNESS', 'Send this case back to the acceptance owner.');
	});

	test('NZBP-GUARD-01 selection alone reads no file contents', async ({ page, baseURL }, info) => {
		await openImport(page, baseURL);
		await selectNzb(page, 'guard-01.nzb', nzbXml(fileXml([segmentXml({ bytes: '10', number: '1', text: 'mid' })])));
		await page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
		await receipt(
			info,
			await engineOf(page),
			'NZBP-GUARD-01',
			{ reads: [], outcome: null },
			async () => ({ reads: await readCalls(page), outcome: (await probe(page)) ?? null }),
			'NZBP_SELECTION_READ',
			'Selection must stay metadata-only; read only after the Preview gesture.'
		);
	});
});

const AX01_FILES = [
	[100000000, 200000000, 300000000],
	[100000000, 34003200]
];
function ax01Xml(): string {
	const body = AX01_FILES.map((sizes, fileIndex) =>
		fileXml(sizes.map((bytes, segIndex) => segmentXml({ bytes: `${bytes}`, number: `${segIndex + 1}`, text: `ax01-f${fileIndex}-s${segIndex}` })), `entry-${fileIndex}`)
	).join('');
	return nzbXml(body);
}
const AX01_SUMMARY = summary({ file_entry_count: 2, segment_count: 5, declared_bytes_total: '734003200', declared_bytes_basis: 'all_segments_claim_bytes' });

test.describe('NZBP happy path and copy', () => {
	test('NZBP-AX-01 choose, preview and aggregate copy', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		await selectNzb(page, 'ax-01.nzb', ax01Xml());
		await (await requirePreview(page, 'NZBP-AX-01')).click();
		const outcome = await settled(page);
		const engine = await engineOf(page);
		await receipt(
			info,
			engine,
			'NZBP-AX-01',
			{
				outcome: { status: 'ok', error_code: null, next_action: 'none', summary: AX01_SUMMARY },
				heading: COPY.heading,
				alwaysLine: COPY.alwaysLine,
				counts: 'This file claims 2 file entries and 5 segments.',
				total: 'Declared size: 700.0 MiB — the sum of the size claims written inside this file.',
				disclaimer: COPY.disclaimer,
				intakeSentence: true,
				warningsAbsent: true
			},
			async () => {
				const main = await page.getByRole('main').innerText();
				return {
					outcome,
					heading: (await page.getByRole('heading', { name: COPY.heading, exact: true }).allTextContents())[0] ?? null,
					alwaysLine: main.includes(COPY.alwaysLine) ? COPY.alwaysLine : null,
					counts: (main.match(/This file claims \d+ file entries and \d+ segments\./) ?? [])[0] ?? null,
					total: (main.match(/Declared size: [^\n]+/) ?? [])[0] ?? null,
					disclaimer: main.includes(COPY.disclaimer) ? COPY.disclaimer : null,
					intakeSentence: main.includes(COPY.intakeSentence),
					warningsAbsent: !/claim no segments|repeats? (a|another) segment/i.test(main)
				};
			},
			'NZBP_HAPPY_PATH',
			FIX_PAGE
		);
		await expectCleanRun(page, nzbp, 'NZBP-AX-01');
	});

	test('NZBP-AX-02 missing size claim renders unknown, never zero', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		const xml = nzbXml(fileXml([segmentXml({ bytes: '100', number: '1', text: 'mid-a' }), segmentXml({ number: '2', text: 'mid-b' })]));
		await selectNzb(page, 'ax-02.nzb', xml);
		await (await requirePreview(page, 'NZBP-AX-02')).click();
		const outcome = await settled(page);
		await receipt(
			info,
			await engineOf(page),
			'NZBP-AX-02',
			{
				outcome: {
					status: 'ok',
					error_code: null,
					next_action: 'none',
					summary: summary({ file_entry_count: 1, segment_count: 2, declared_bytes_total: null, declared_bytes_basis: 'unknown', segments_without_byte_claim: 1 })
				},
				total: 'Declared size: unknown — 1 segment does not state a size, so no total is shown.'
			},
			async () => ({
				outcome,
				total: ((await page.getByRole('main').innerText()).match(/Declared size: [^\n]+/) ?? [])[0] ?? null
			}),
			'NZBP_UNKNOWN',
			FIX_PAGE
		);
		await expectCleanRun(page, nzbp, 'NZBP-AX-02');
	});

	test('NZBP-AX-03 document without entries renders the no-segments line', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		await selectNzb(page, 'ax-03.nzb', nzbXml(''));
		await (await requirePreview(page, 'NZBP-AX-03')).click();
		const outcome = await settled(page);
		await receipt(
			info,
			await engineOf(page),
			'NZBP-AX-03',
			{
				outcome: { status: 'ok', error_code: null, next_action: 'none', summary: summary() },
				total: 'Declared size: unknown — this file claims no segments.'
			},
			async () => ({
				outcome,
				total: ((await page.getByRole('main').innerText()).match(/Declared size: [^\n]+/) ?? [])[0] ?? null
			}),
			'NZBP_UNKNOWN',
			FIX_PAGE
		);
		await expectCleanRun(page, nzbp, 'NZBP-AX-03');
	});

	test('NZBP-AX-07B bare doctype previews without fetching', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		const xml = nzbXml(fileXml([segmentXml({ bytes: '10', number: '1', text: 'mid' })]), {
			prolog: '<!DOCTYPE nzb PUBLIC "-//nzbp//DTD NZB 1.1//EN" "http://nzbp-sentinel.invalid/nzb-1.1.dtd">'
		});
		await selectNzb(page, 'ax-07b.nzb', xml);
		const before = nzbp.requests.length;
		await (await requirePreview(page, 'NZBP-AX-07B')).click();
		const outcome = await settled(page);
		await receipt(
			info,
			await engineOf(page),
			'NZBP-AX-07B',
			{
				outcome: {
					status: 'ok',
					error_code: null,
					next_action: 'none',
					summary: summary({ file_entry_count: 1, segment_count: 1, declared_bytes_total: '10', declared_bytes_basis: 'all_segments_claim_bytes' })
				},
				postGestureRequests: []
			},
			async () => ({ outcome, postGestureRequests: nzbp.requests.slice(before) }),
			'NZBP_DOCTYPE',
			FIX_PAGE
		);
		await expectCleanRun(page, nzbp, 'NZBP-AX-07B');
	});

	test('NZBP-COPY-05 warnings render exactly when counters are positive', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		const body =
			'<file subject="lonely-a"></file><file subject="lonely-b"><segments></segments></file>' +
			fileXml(
				[
					segmentXml({ bytes: '1', number: '5', text: 'warn-mid-a' }),
					segmentXml({ bytes: '1', number: '5', text: 'warn-mid-b' }),
					segmentXml({ bytes: '1', number: '5', text: 'warn-mid-a' }),
					segmentXml({ bytes: '1', number: '5', text: 'warn-mid-b' })
				],
				'warn-entry'
			);
		await selectNzb(page, 'copy-05.nzb', nzbXml(body));
		await (await requirePreview(page, 'NZBP-COPY-05')).click();
		const outcome = await settled(page);
		await receipt(
			info,
			await engineOf(page),
			'NZBP-COPY-05',
			{
				outcome: {
					status: 'ok',
					error_code: null,
					next_action: 'none',
					summary: summary({
						file_entry_count: 3,
						segment_count: 4,
						declared_bytes_total: '4',
						declared_bytes_basis: 'all_segments_claim_bytes',
						duplicate_segment_numbers: 3,
						duplicate_message_ids: 2,
						files_without_segments: 2
					})
				},
				warnings: ['2 file entries claim no segments.', '3 segments repeat a segment number inside their file entry.', "2 segments repeat another segment's message identifier."]
			},
			async () => {
				const main = await page.getByRole('main').innerText();
				const warnings = [
					'2 file entries claim no segments.',
					'3 segments repeat a segment number inside their file entry.',
					"2 segments repeat another segment's message identifier."
				];
				return { outcome, warnings: warnings.filter((line) => main.includes(line)) };
			},
			'NZBP_WARNINGS',
			FIX_PAGE
		);
		await expectCleanRun(page, nzbp, 'NZBP-COPY-05');
	});

	test('NZBP-COPY-06 forbidden words stay out of preview strings', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		await selectNzb(page, 'copy-06.nzb', ax01Xml());
		await (await requirePreview(page, 'NZBP-COPY-06')).click();
		await settled(page);
		await receipt(
			info,
			await engineOf(page),
			'NZBP-COPY-06',
			{ forbiddenHits: [] },
			async () => {
				const section = page.locator('section', { has: page.getByRole('heading', { name: COPY.heading, exact: true }) });
				const text = (await section.first().innerText()).replace(COPY.disclaimer, '').replace(COPY.intakeSentence, '');
				return { forbiddenHits: FORBIDDEN.filter((pattern) => pattern.test(text)).map((pattern) => pattern.source) };
			},
			'NZBP_COPY',
			FIX_PAGE
		);
		await expectCleanRun(page, nzbp, 'NZBP-COPY-06');
	});

	test('NZBP-COPY-07 unknown totals never render as zero', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		const xml = nzbXml(fileXml([segmentXml({ bytes: '100', number: '1', text: 'mid-a' }), segmentXml({ number: '2', text: 'mid-b' })]));
		await selectNzb(page, 'copy-07.nzb', xml);
		await (await requirePreview(page, 'NZBP-COPY-07')).click();
		await settled(page);
		await receipt(
			info,
			await engineOf(page),
			'NZBP-COPY-07',
			{ zeroRendered: false },
			async () => {
				const section = page.locator('section', { has: page.getByRole('heading', { name: COPY.heading, exact: true }) });
				const text = await section.first().innerText();
				return { zeroRendered: /\b0\b/.test(text) || /0\s*(B|bytes|KiB|MiB|GiB)/i.test(text) };
			},
			'NZBP_COPY',
			FIX_PAGE
		);
		await expectCleanRun(page, nzbp, 'NZBP-COPY-07');
	});

	test('NZBP-COPY-08 page and meta separate selection from the explicit local read', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		await receipt(
			info,
			await engineOf(page),
			'NZBP-COPY-08',
			{ importStays: true, selectionIsMetadata: true, previewIsExplicitLocalRead: true, metaNamesPreview: true, metaDropsFalseProcessingClaim: true },
			async () => {
				const main = await page.getByRole('main').innerText();
				const meta = (await page.locator('meta[name="description"]').getAttribute('content')) ?? '';
				return {
					importStays: main.includes(COPY.importUnavailable),
					selectionIsMetadata: /metadata only/i.test(main),
					previewIsExplicitLocalRead: /explicit/i.test(main) && /local read/i.test(main),
					metaNamesPreview: /preview/i.test(meta) && /local/i.test(meta),
					metaDropsFalseProcessingClaim: !/does not process/i.test(meta) && !/no upload, processing/i.test(meta)
				};
			},
			'NZBP_COPY',
			FIX_PAGE
		);
		await expectCleanRun(page, nzbp, 'NZBP-COPY-08');
	});
});

test.describe('NZBP stable error outcomes', () => {
	async function errorCase(page: Page, caseId: string, name: string, content: string | Buffer, code: string, info: TestInfo, engine: string, extraMarkers: string[] = []): Promise<void> {
		await selectNzb(page, name, content);
		await (await requirePreview(page, caseId)).click();
		const outcome = await settled(page);
		await receipt(
			info,
			engine,
			caseId,
			{ outcome: errOutcome(code), feedback: USER_TEXT[code] },
			async () => ({
				outcome,
				feedback: ((await page.getByTestId('import-status').allTextContents()).join('\n').match(new RegExp(escapeRegExp(USER_TEXT[code]))) ?? [])[0] ?? null
			}),
			'NZBP_ERROR_TEXT',
			FIX_PAGE
		);
		const main = await page.getByRole('main').innerText();
		expect.soft(main, `${caseId}: no summary heading on error`).not.toContain(COPY.heading);
		if (extraMarkers.length > 0) {
			for (const marker of extraMarkers) {
				expect.soft(main, `${caseId}: no echo of ${marker}`).not.toContain(marker);
				expect.soft(JSON.stringify(outcome), `${caseId}: no echo in outcome JSON`).not.toContain(marker);
			}
		}
	}

	function escapeRegExp(text: string): string {
		return text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
	}

	test('NZBP-ERR-03 stream failure maps to try-again without the caught message', async ({ page, baseURL, nzbp }, info) => {
		await page.addInitScript(() => {
			Blob.prototype.stream = () => new ReadableStream({ start(controller) { controller.error(new Error('NZBP-STREAM-FAIL-marker')); } });
		});
		await openImport(page, baseURL);
		const engine = await engineOf(page);
		await errorCase(page, 'NZBP-ERR-03', 'err-03.nzb', nzbXml(fileXml([segmentXml({ bytes: '10', number: '1', text: 'mid' })])), 'NZBP_READ_FAILED', info, engine, [
			'NZBP-STREAM-FAIL-marker'
		]);
		expect.soft(nzbp.consoleMessages.map((entry) => entry.text).join('\n'), 'NZBP-ERR-03: caught message never logged').not.toContain('NZBP-STREAM-FAIL-marker');
		await expectCleanRun(page, nzbp, 'NZBP-ERR-03');
	});

	test('NZBP-ERR-04 invalid UTF-8 bytes are rejected', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		const engine = await engineOf(page);
		const buffer = Buffer.concat([Buffer.from('<?xml version="1.0"?><nzb>'), Buffer.from([0xff]), Buffer.from('</nzb>')]);
		await errorCase(page, 'NZBP-ERR-04', 'err-04.nzb', buffer, 'NZBP_ENCODING_UNSUPPORTED', info, engine);
		await expectCleanRun(page, nzbp, 'NZBP-ERR-04');
	});

	test('NZBP-ERR-04B XML 1.1 declaration is rejected', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		const engine = await engineOf(page);
		await errorCase(page, 'NZBP-ERR-04B', 'err-04b.nzb', nzbXml('', { declaration: '<?xml version="1.1"?>' }), 'NZBP_ENCODING_UNSUPPORTED', info, engine);
		await expectCleanRun(page, nzbp, 'NZBP-ERR-04B');
	});

	test('NZBP-ERR-04C non-UTF-8 declared encoding is rejected', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		const engine = await engineOf(page);
		await errorCase(
			page,
			'NZBP-ERR-04C',
			'err-04c.nzb',
			nzbXml('', { declaration: '<?xml version="1.0" encoding="ISO-8859-1"?>' }),
			'NZBP_ENCODING_UNSUPPORTED',
			info,
			engine
		);
		await expectCleanRun(page, nzbp, 'NZBP-ERR-04C');
	});

	test('NZBP-ERR-05 internal subset is rejected', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		const engine = await engineOf(page);
		const xml = `<?xml version="1.0"?><!DOCTYPE nzb [<!ENTITY a "x">]><nzb>${fileXml([segmentXml({ bytes: '10', number: '1', text: 'mid' })])}</nzb>`;
		await errorCase(page, 'NZBP-ERR-05', 'err-05.nzb', xml, 'NZBP_DOCTYPE_NOT_ALLOWED', info, engine);
		await expectCleanRun(page, nzbp, 'NZBP-ERR-05');
	});

	test('NZBP-ERR-06 malformed markup maps to the constant without excerpts', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		const engine = await engineOf(page);
		const marker = 'NZBP-SENTINEL-MALFORMED-aa92';
		const xml = `<nzb><file subject="${marker}"><segments><segment bytes="10">x</seg></segment></segments></file></nzb>`;
		await selectNzb(page, 'err-06.nzb', xml);
		await (await requirePreview(page, 'NZBP-ERR-06')).click();
		const outcome = await settled(page);
		await receipt(
			info,
			engine,
			'NZBP-ERR-06',
			{ outcome: errOutcome('NZBP_MALFORMED_XML'), feedback: USER_TEXT['NZBP_MALFORMED_XML'] },
			async () => ({
				outcome,
				feedback: ((await page.getByTestId('import-status').allTextContents()).join('\n').match(/This file is not readable XML\./) ?? [])[0] ?? null
			}),
			'NZBP_ERROR_TEXT',
			FIX_PAGE
		);
		expectNoEcho(await surfaces(page, nzbp), [marker], 'NZBP-ERR-06');
		await expectCleanRun(page, nzbp, 'NZBP-ERR-06');
	});

	test('NZBP-ERR-07 non-nzb root is rejected', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		const engine = await engineOf(page);
		await errorCase(page, 'NZBP-ERR-07', 'err-07.nzb', '<?xml version="1.0"?><rss></rss>', 'NZBP_NOT_NZB', info, engine);
		await expectCleanRun(page, nzbp, 'NZBP-ERR-07');
	});

	test('NZBP-ERR-07B foreign nzb namespace is rejected', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		const engine = await engineOf(page);
		await errorCase(page, 'NZBP-ERR-07B', 'err-07b.nzb', '<?xml version="1.0"?><nzb xmlns="http://foreign.invalid/nzb"></nzb>', 'NZBP_NOT_NZB', info, engine);
		await expectCleanRun(page, nzbp, 'NZBP-ERR-07B');
	});

	test('NZBP-ERR-08 5001 file entries fail', async ({ page, baseURL, nzbp }, info) => {
		test.slow();
		await openImport(page, baseURL);
		const engine = await engineOf(page);
		await errorCase(page, 'NZBP-ERR-08', 'err-08.nzb', nzbXml('<file></file>'.repeat(5001)), 'NZBP_TOO_MANY_FILES', info, engine);
		await expectCleanRun(page, nzbp, 'NZBP-ERR-08');
	});

	test('NZBP-ERR-09 60001 segments fail', async ({ page, baseURL, nzbp }, info) => {
		test.slow();
		await openImport(page, baseURL);
		const engine = await engineOf(page);
		await errorCase(page, 'NZBP-ERR-09', 'err-09.nzb', nzbXml(fileXml([segmentXml({ bytes: '1', number: '1', text: 'bulk' })]).replace('bulk</segment>', `bulk</segment>${'<segment bytes="1">x</segment>'.repeat(60000)}`)), 'NZBP_TOO_MANY_SEGMENTS', info, engine);
		await expectCleanRun(page, nzbp, 'NZBP-ERR-09');
	});

	test('NZBP-ERR-10 depth 17 fails', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		const engine = await engineOf(page);
		await errorCase(page, 'NZBP-ERR-10', 'err-10.nzb', nzbXml(`${'<wrap>'.repeat(16)}<leaf/>${'</wrap>'.repeat(16)}`), 'NZBP_TOO_DEEP', info, engine);
		await expectCleanRun(page, nzbp, 'NZBP-ERR-10');
	});

	test('NZBP-ERR-11 17 attributes on one element fail', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		const engine = await engineOf(page);
		const attrs = Array.from({ length: 17 }, (_, index) => `k${index}="v"`).join(' ');
		await errorCase(page, 'NZBP-ERR-11', 'err-11.nzb', nzbXml(`<meta ${attrs}/>`), 'NZBP_TOO_MANY_NODES', info, engine);
		await expectCleanRun(page, nzbp, 'NZBP-ERR-11');
	});

	test('NZBP-ERR-12 over-cap declared bytes fail', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		const engine = await engineOf(page);
		await errorCase(
			page,
			'NZBP-ERR-12',
			'err-12.nzb',
			nzbXml(fileXml([segmentXml({ bytes: '99999999999999999999', number: '1', text: 'mid-big' })])),
			'NZBP_DECLARED_BYTES_OVER_CAP',
			info,
			engine,
			['mid-big', '99999999999999999999']
		);
		await expectCleanRun(page, nzbp, 'NZBP-ERR-12');
	});

	test('NZBP-ERR-13 missing parser maps to the unavailable outcome', async ({ page, baseURL, nzbp }, info) => {
		await page.addInitScript(() => {
			delete (window as unknown as { DOMParser?: unknown }).DOMParser;
		});
		await openImport(page, baseURL);
		const engine = await engineOf(page);
		await errorCase(page, 'NZBP-ERR-13', 'err-13.nzb', nzbXml(fileXml([segmentXml({ bytes: '10', number: '1', text: 'mid' })])), 'NZBP_PARSER_UNAVAILABLE', info, engine);
		await expectCleanRun(page, nzbp, 'NZBP-ERR-13');
	});

	test('NZBP-R1-11B lowercase doctype token is rejected by the parser', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		const engine = await engineOf(page);
		await errorCase(page, 'NZBP-R1-11B', 'r1-11b.nzb', '<!doctype nzb><nzb></nzb>', 'NZBP_MALFORMED_XML', info, engine);
		await expectCleanRun(page, nzbp, 'NZBP-R1-11B');
	});
});

test.describe('NZBP numeric and counting parity in the browser', () => {
	test('NZBP-R3-03 reference-encoded digits count as their expansion', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		await selectNzb(page, 'r3-03.nzb', nzbXml(fileXml([segmentXml({ bytes: '&#49;&#50;&#51;', number: '1', text: 'mid-ref' })])));
		await (await requirePreview(page, 'NZBP-R3-03')).click();
		const outcome = await settled(page);
		await receipt(
			info,
			await engineOf(page),
			'NZBP-R3-03',
			{
				outcome: {
					status: 'ok',
					error_code: null,
					next_action: 'none',
					summary: summary({ file_entry_count: 1, segment_count: 1, declared_bytes_total: '123', declared_bytes_basis: 'all_segments_claim_bytes' })
				}
			},
			async () => ({ outcome }),
			'NZBP_REFERENCES',
			FIX_PAGE
		);
		await expectCleanRun(page, nzbp, 'NZBP-R3-03');
	});

	test('NZBP-R3-04 references expanding past 1024 chars fail before numbers run', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		const expanded = '&#65;'.repeat(1100);
		await selectNzb(page, 'r3-04.nzb', nzbXml(fileXml([segmentXml({ bytes: expanded, number: '1', text: 'mid-ref' })])));
		await (await requirePreview(page, 'NZBP-R3-04')).click();
		const outcome = await settled(page);
		await receipt(info, await engineOf(page), 'NZBP-R3-04', { outcome: errOutcome('NZBP_TOO_MANY_NODES') }, async () => ({ outcome }), 'NZBP_REFERENCES', FIX_PAGE);
		await expectCleanRun(page, nzbp, 'NZBP-R3-04');
	});

	test('NZBP-R4-10B prefixed elements count, uppercase files do not', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		const prefixed = '<p:file xmlns:p="urn:nzbp-test"><p:segments><p:segment bytes="1" number="1">mid-p</p:segment></p:segments></p:file>';
		await selectNzb(page, 'r4-10b-a.nzb', nzbXml(prefixed));
		await (await requirePreview(page, 'NZBP-R4-10B')).click();
		const first = await settled(page);
		await selectNzb(page, 'r4-10b-b.nzb', nzbXml('<FILE><segments><segment bytes="1" number="1">mid-u</segment></segments></FILE>'));
		await (await requirePreview(page, 'NZBP-R4-10B')).click();
		const second = await settled(page);
		await receipt(
			info,
			await engineOf(page),
			'NZBP-R4-10B',
			{
				prefixed: { status: 'ok', error_code: null, next_action: 'none', summary: summary({ file_entry_count: 1, segment_count: 1, declared_bytes_total: '1', declared_bytes_basis: 'all_segments_claim_bytes' }) },
				upper: { status: 'ok', error_code: null, next_action: 'none', summary: summary({ file_entry_count: 0, segment_count: 1, declared_bytes_total: '1', declared_bytes_basis: 'all_segments_claim_bytes' }) }
			},
			async () => ({ prefixed: first, upper: second }),
			'NZBP_COUNTING',
			FIX_PAGE
		);
		await expectCleanRun(page, nzbp, 'NZBP-R4-10B');
	});

	test('NZBP-R4-11B prefixed bytes attribute is not a size claim', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		await selectNzb(page, 'r4-11b.nzb', nzbXml(fileXml([segmentXml({ text: 'mid-p', extra: ' p:bytes="100" xmlns:p="urn:nzbp-test"' })])));
		await (await requirePreview(page, 'NZBP-R4-11B')).click();
		const outcome = await settled(page);
		await receipt(
			info,
			await engineOf(page),
			'NZBP-R4-11B',
			{
				outcome: {
					status: 'ok',
					error_code: null,
					next_action: 'none',
					summary: summary({ file_entry_count: 1, segment_count: 1, declared_bytes_total: null, declared_bytes_basis: 'unknown', segments_without_byte_claim: 1, segments_without_number: 1 })
				}
			},
			async () => ({ outcome }),
			'NZBP_COUNTING',
			FIX_PAGE
		);
		await expectCleanRun(page, nzbp, 'NZBP-R4-11B');
	});

	test('NZBP-R4-17 segment identity is the concatenated descendant text', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		const body = fileXml([`<segment bytes="1" number="1">ab<nested/>cd</segment>`, segmentXml({ bytes: '1', number: '2', text: 'abcd' })].join(''));
		await selectNzb(page, 'r4-17.nzb', nzbXml(body));
		await (await requirePreview(page, 'NZBP-R4-17')).click();
		const outcome = await settled(page);
		await receipt(
			info,
			await engineOf(page),
			'NZBP-R4-17',
			{
				outcome: {
					status: 'ok',
					error_code: null,
					next_action: 'none',
					summary: summary({ file_entry_count: 1, segment_count: 2, declared_bytes_total: '2', declared_bytes_basis: 'all_segments_claim_bytes', duplicate_message_ids: 1 })
				}
			},
			async () => ({ outcome }),
			'NZBP_COUNTING',
			FIX_PAGE
		);
		await expectCleanRun(page, nzbp, 'NZBP-R4-17');
	});

	test('NZBP-R4-READ-01 zero actual bytes preview as empty despite the selected size', async ({ page, baseURL, nzbp }, info) => {
		await page.addInitScript(() => {
			Blob.prototype.stream = () => new ReadableStream({ start(controller) { controller.close(); } });
		});
		await openImport(page, baseURL);
		await selectNzb(page, 'r4-read-01.nzb', nzbXml(fileXml([segmentXml({ bytes: '10', number: '1', text: 'mid' })])));
		await (await requirePreview(page, 'NZBP-R4-READ-01')).click();
		const outcome = await settled(page);
		await receipt(
			info,
			await engineOf(page),
			'NZBP-R4-READ-01',
			{ outcome: errOutcome('NZBP_EMPTY'), feedback: USER_TEXT['NZBP_EMPTY'] },
			async () => ({
				outcome,
				feedback: ((await page.getByTestId('import-status').allTextContents()).join('\n').match(/This file is empty, so there is nothing to preview\./) ?? [])[0] ?? null
			}),
			'NZBP_ACTUAL_BYTES',
			FIX_PAGE
		);
		await expectCleanRun(page, nzbp, 'NZBP-R4-READ-01');
	});

	test('NZBP-R4-READ-02 over-cap actual bytes fail despite the selected size', async ({ page, baseURL, nzbp }, info) => {
		await page.addInitScript(() => {
			const original = Blob.prototype.stream;
			Blob.prototype.stream = function (this: Blob) {
				const source = original.call(this);
				return new ReadableStream({
					async start(controller) {
						const reader = source.getReader();
						try {
							for (;;) {
								const { done, value } = await reader.read();
								if (done) break;
								controller.enqueue(value);
							}
						} finally {
							reader.releaseLock();
						}
						controller.enqueue(new Uint8Array(2 * 1024 * 1024 + 1));
						controller.close();
					}
				});
			};
		});
		await openImport(page, baseURL);
		await selectNzb(page, 'r4-read-02.nzb', nzbXml(fileXml([segmentXml({ bytes: '10', number: '1', text: 'mid' })])));
		await (await requirePreview(page, 'NZBP-R4-READ-02')).click();
		const outcome = await settled(page);
		await receipt(
			info,
			await engineOf(page),
			'NZBP-R4-READ-02',
			{ outcome: errOutcome('NZBP_TOO_LARGE'), feedback: USER_TEXT['NZBP_TOO_LARGE'] },
			async () => ({
				outcome,
				feedback: ((await page.getByTestId('import-status').allTextContents()).join('\n').match(/This file is larger than the 2 MiB preview limit\./) ?? [])[0] ?? null
			}),
			'NZBP_ACTUAL_BYTES',
			FIX_PAGE
		);
		await expectCleanRun(page, nzbp, 'NZBP-R4-READ-02');
	});
});

test.describe('NZBP no-echo, network and storage posture', () => {
	test('NZBP-R5-10 hidden-region sentinels surface nowhere', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		const xml =
			'<?xml version="1.0"?><!DOCTYPE nzb SYSTEM "http://NZBP-SENTINEL-DTD-55f0.invalid/nzb.dtd">' +
			`<!-- NZBP-SENTINEL-COMMENT-6a01 --><?nzbp NZBP-SENTINEL-PI-83e4?><nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">` +
			`<file poster="NZBP-SENTINEL-POSTER-9c1e" date="1" subject="NZBP-SENTINEL-SUBJECT-7f3a">` +
			`<groups><group>NZBP-SENTINEL-GROUP-4b77</group></groups><segments>` +
			`<segment bytes="10" number="1"><![CDATA[NZBP-SENTINEL-CDATA-11bc]]></segment>` +
			`<segment bytes="20" number="2">NZBP-SENTINEL-MID-2d8f</segment>` +
			`</segments></file></nzb>`;
		await selectNzb(page, 'r5-10.nzb', xml);
		await (await requirePreview(page, 'NZBP-R5-10')).click();
		const outcome = await settled(page);
		const engine = await engineOf(page);
		await receipt(
			info,
			engine,
			'NZBP-R5-10',
			{
				outcome: {
					status: 'ok',
					error_code: null,
					next_action: 'none',
					summary: summary({ file_entry_count: 1, segment_count: 2, declared_bytes_total: '30', declared_bytes_basis: 'all_segments_claim_bytes' })
				}
			},
			async () => ({ outcome }),
			'NZBP_NO_ECHO',
			FIX_PAGE
		);
		expectNoEcho(await surfaces(page, nzbp), SENTINELS, 'NZBP-R5-10');
		await expectCleanRun(page, nzbp, 'NZBP-R5-10');
	});

	test('NZBP-R5-12 hostile reference shapes cause zero requests', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		const engine = await engineOf(page);
		const shapes: [string, string][] = [
			['external-dtd', nzbXml(fileXml([segmentXml({ bytes: '10', number: '1', text: 'mid' })]), { prolog: '<!DOCTYPE nzb SYSTEM "http://nzbp-sentinel.invalid/x.dtd">' })],
			['stylesheet-pi', '<?xml version="1.0"?><?xml-stylesheet href="http://nzbp-sentinel.invalid/x.xsl"?><nzb></nzb>'],
			['xinclude', nzbXml('<xi:include href="http://nzbp-sentinel.invalid/x.xml" xmlns:xi="http://www.w3.org/2001/XInclude"/>')]
		];
		const postGesture: Record<string, string[]> = {};
		for (const [name, xml] of shapes) {
			await selectNzb(page, `r5-12-${name}.nzb`, xml);
			const before = nzbp.requests.length;
			await (await requirePreview(page, 'NZBP-R5-12')).click();
			await settled(page);
			postGesture[name] = nzbp.requests.slice(before);
		}
		await receipt(info, engine, 'NZBP-R5-12', { postGesture: { 'external-dtd': [], 'stylesheet-pi': [], xinclude: [] }, unsafe: [] }, async () => ({ postGesture, unsafe: nzbp.unsafeRequests }), 'NZBP_NETWORK', FIX_PAGE);
		await expectCleanRun(page, nzbp, 'NZBP-R5-12');
	});

	test('NZBP-R5-13 preview writes no storage', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		await selectNzb(page, 'r5-13.nzb', ax01Xml());
		const before = await storageSnapshot(page);
		await (await requirePreview(page, 'NZBP-R5-13')).click();
		await settled(page);
		await receipt(info, await engineOf(page), 'NZBP-R5-13', { storageChanged: false }, async () => ({ storageChanged: JSON.stringify(await storageSnapshot(page)) !== JSON.stringify(before) }), 'NZBP_STORAGE', FIX_PAGE);
		await expectCleanRun(page, nzbp, 'NZBP-R5-13');
	});

	test('NZBP-R5-14 malformed input attaches no parser nodes', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		await selectNzb(page, 'r5-14.nzb', '<nzb><file><segments><segment>x</seg></segment></segments></file></nzb>');
		await (await requirePreview(page, 'NZBP-R5-14')).click();
		const outcome = await settled(page);
		await receipt(
			info,
			await engineOf(page),
			'NZBP-R5-14',
			{ outcome: errOutcome('NZBP_MALFORMED_XML'), attached: 0 },
			async () => ({ outcome, attached: await page.evaluate(() => document.getElementsByTagName('parsererror').length) }),
			'NZBP_NO_ECHO',
			FIX_PAGE
		);
		await expectCleanRun(page, nzbp, 'NZBP-R5-14');
	});

	test('NZBP-R5-15 console carries no summary payload', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		await selectNzb(page, 'r5-15.nzb', ax01Xml());
		await (await requirePreview(page, 'NZBP-R5-15')).click();
		await settled(page);
		await receipt(
			info,
			await engineOf(page),
			'NZBP-R5-15',
			{ leaked: [] },
			async () => ({ leaked: ['file_entry_count', 'segment_count', 'declared_bytes_total'].filter((key) => nzbp.consoleMessages.some((entry) => entry.text.includes(key))) }),
			'NZBP_NO_ECHO',
			FIX_PAGE
		);
		await expectCleanRun(page, nzbp, 'NZBP-R5-15');
	});
});

function largeValidXml(segmentCount: number): string {
	return nzbXml(fileXml(Array.from({ length: segmentCount }, (_, index) => segmentXml({ bytes: '100', number: `${index + 1}`, text: `bulk-mid-${index}` })), 'bulk-entry'));
}

async function holdNextStreamRead(page: Page) {
	const gate = await page.evaluateHandle(() => {
		let entered!: () => void;
		let release!: () => void;
		const started = new Promise<void>((resolve) => { entered = resolve; });
		const held = new Promise<void>((resolve) => { release = resolve; });
		const originalStream = Blob.prototype.stream;
		Blob.prototype.stream = function (this: Blob) {
			Blob.prototype.stream = originalStream;
			const stream = originalStream.call(this);
			const getReader = stream.getReader.bind(stream);
			Object.defineProperty(stream, 'getReader', {
				value() {
					const reader = getReader();
					const read = reader.read.bind(reader);
					reader.read = async () => {
						reader.read = read;
						// Hold only the delivery: native bytes, cancel and release stay real.
						const result = await read();
						entered();
						await held;
						return result;
					};
					return reader;
				}
			});
			return stream;
		};
		return { started, release };
	});
	return {
		waitUntilReading: () => gate.evaluate((state) => state.started),
		async release() {
			await gate.evaluate((state) => state.release());
			await gate.dispose();
		}
	};
}

test.describe('NZBP cancellation and staleness', () => {
	test('NZBP-R6-01 cancel during the read discards and releases the reader', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		await selectNzb(page, 'r6-01.nzb', largeValidXml(20000));
		const read = await holdNextStreamRead(page);
		await (await requirePreview(page, 'NZBP-R6-01')).click();
		await read.waitUntilReading();
		await expect(page.getByTestId('import-status'), 'NZBP-R6-01: local read is announced').toContainText(/Reading the file locally…/, { timeout: 5000 });
		await cancelButton(page).click();
		await read.release();
		const outcome = await settled(page);
		const engine = await engineOf(page);
		await receipt(
			info,
			engine,
			'NZBP-R6-01',
			{ outcome: { status: 'cancelled' }, cancelledLine: true, summaryAbsent: true },
			async () => {
				const main = await page.getByRole('main').innerText();
				return {
					outcome,
					cancelledLine: /Preview cancelled — (nothing was read|the read was stopped)\./.test(main),
					summaryAbsent: !main.includes(COPY.heading)
				};
			},
			'NZBP_CANCEL',
			FIX_PAGE
		);
		// The released reader allows an immediate re-preview of the same file.
		await (await requirePreview(page, 'NZBP-R6-01')).click();
		const retried = await settled(page);
		expect((retried as { status?: unknown }).status, 'NZBP-R6-01: reader released for retry').toBe('ok');
		await expectCleanRun(page, nzbp, 'NZBP-R6-01');
	});

	test('NZBP-R6-02 replacing the selection discards the in-flight result', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		await selectNzb(page, 'r6-02-a.nzb', largeValidXml(20000));
		await (await requirePreview(page, 'NZBP-R6-02')).click();
		await selectNzb(page, 'r6-02-b.nzb', nzbXml(fileXml([segmentXml({ bytes: '10', number: '1', text: 'mid-b' })])));
		await page.waitForTimeout(1000);
		const engine = await engineOf(page);
		await receipt(
			info,
			engine,
			'NZBP-R6-02',
			{ outcome: null, summaryAbsent: true, showsReplacement: true },
			async () => {
				const main = await page.getByRole('main').innerText();
				return { outcome: (await probe(page)) ?? null, summaryAbsent: !main.includes(COPY.heading), showsReplacement: main.includes('r6-02-b.nzb') };
			},
			'NZBP_STALE',
			FIX_PAGE
		);
		await (await requirePreview(page, 'NZBP-R6-02')).click();
		const outcome = await settled(page);
		expect(outcome, 'NZBP-R6-02: replacement previews on its own generation').toEqual({
			status: 'ok',
			error_code: null,
			next_action: 'none',
			summary: summary({ file_entry_count: 1, segment_count: 1, declared_bytes_total: '10', declared_bytes_basis: 'all_segments_claim_bytes' })
		});
		await expectCleanRun(page, nzbp, 'NZBP-R6-02');
	});

	test('NZBP-R6-02B replacing via drop discards the in-flight result', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		await selectNzb(page, 'r6-02b-a.nzb', largeValidXml(20000));
		await (await requirePreview(page, 'NZBP-R6-02B')).click();
		const dropped = nzbXml(fileXml([segmentXml({ bytes: '10', number: '1', text: 'mid-drop' })]));
		await page.getByTestId('import-drop-area').evaluate((area, text) => {
			const transfer = new DataTransfer();
			transfer.items.add(new File([text], 'r6-02b-b.nzb', { type: 'application/x-nzb' }));
			area.dispatchEvent(new DragEvent('drop', { dataTransfer: transfer, bubbles: true, cancelable: true }));
		}, dropped);
		await expect(page.getByTestId('import-status')).toContainText(/r6-02b-b\.nzb.*selected locally/i);
		await page.waitForTimeout(1000);
		await receipt(
			info,
			await engineOf(page),
			'NZBP-R6-02B',
			{ outcome: null, summaryAbsent: true },
			async () => ({ outcome: (await probe(page)) ?? null, summaryAbsent: !(await page.getByRole('main').innerText()).includes(COPY.heading) }),
			'NZBP_STALE',
			FIX_PAGE
		);
		await expectCleanRun(page, nzbp, 'NZBP-R6-02B');
	});

	test('NZBP-R6-03 same metadata with different bytes still invalidates', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		const stamp = 1700000000000;
		const first = largeValidXml(20000);
		const secondSmall = nzbXml(fileXml([segmentXml({ bytes: '10', number: '1', text: 'mid-second' })]));
		const padding = first.length - secondSmall.length;
		expect(padding, 'NZBP-R6-03: replacement fixture pads to the same size').toBeGreaterThan(100);
		const second = secondSmall.replace('</nzb>', `<!--${'p'.repeat(padding - 7)}--></nzb>`);
		expect(Buffer.byteLength(second), 'NZBP-R6-03: same byte size').toBe(Buffer.byteLength(first));
		await setFileWithMeta(page, 'r6-03.nzb', [...Buffer.from(first)], stamp);
		await expect(page.getByTestId('import-status')).toContainText(/selected locally/i);
		await (await requirePreview(page, 'NZBP-R6-03')).click();
		await setFileWithMeta(page, 'r6-03.nzb', [...Buffer.from(second)], stamp);
		await expect(page.getByTestId('import-status')).toContainText(/selected locally/i);
		await page.waitForTimeout(1000);
		const engine = await engineOf(page);
		await receipt(
			info,
			engine,
			'NZBP-R6-03',
			{ outcome: null, summaryAbsent: true },
			async () => ({ outcome: (await probe(page)) ?? null, summaryAbsent: !(await page.getByRole('main').innerText()).includes(COPY.heading) }),
			'NZBP_STALE',
			FIX_PAGE
		);
		await (await requirePreview(page, 'NZBP-R6-03')).click();
		const outcome = await settled(page);
		expect(outcome, 'NZBP-R6-03: second file previews on its own generation').toEqual({
			status: 'ok',
			error_code: null,
			next_action: 'none',
			summary: summary({ file_entry_count: 1, segment_count: 1, declared_bytes_total: '10', declared_bytes_basis: 'all_segments_claim_bytes' })
		});
		await expectCleanRun(page, nzbp, 'NZBP-R6-03');
	});

	test('NZBP-R6-04 navigating away mid-read is safe', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		await selectNzb(page, 'r6-04.nzb', largeValidXml(20000));
		await (await requirePreview(page, 'NZBP-R6-04')).click();
		await page.goto('/library');
		await expect(page.getByRole('main'), 'NZBP-R6-04: destination renders').toBeVisible();
		await receipt(info, await engineOf(page), 'NZBP-R6-04', { pageErrors: [] }, async () => ({ pageErrors: nzbp.pageErrors }), 'NZBP_TEARDOWN', FIX_PAGE);
	});

	test('NZBP-R6-05 Escape cancels the read', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		await selectNzb(page, 'r6-05.nzb', largeValidXml(20000));
		await (await requirePreview(page, 'NZBP-R6-05')).click();
		await page.keyboard.press('Escape');
		const outcome = await settled(page);
		await receipt(
			info,
			await engineOf(page),
			'NZBP-R6-05',
			{ outcome: { status: 'cancelled' }, summaryAbsent: true },
			async () => ({ outcome, summaryAbsent: !(await page.getByRole('main').innerText()).includes(COPY.heading) }),
			'NZBP_CANCEL',
			FIX_PAGE
		);
		await expectCleanRun(page, nzbp, 'NZBP-R6-05');
	});

	test('NZBP-R6-06 Clear during the read drops everything', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		await selectNzb(page, 'r6-06.nzb', largeValidXml(20000));
		await (await requirePreview(page, 'NZBP-R6-06')).click();
		await page.getByRole('button', { name: 'Clear selection', exact: true }).click();
		await page.waitForTimeout(1000);
		await receipt(
			info,
			await engineOf(page),
			'NZBP-R6-06',
			{ outcome: null, inputEmpty: true, feedbackGone: true, summaryAbsent: true, previewDisabled: true },
			async () => ({
				outcome: (await probe(page)) ?? null,
				inputEmpty: (await page.getByTestId('import-file').evaluate((element: HTMLInputElement) => element.value)) === '',
				feedbackGone: (await page.getByTestId('import-status').count()) === 0,
				summaryAbsent: !(await page.getByRole('main').innerText()).includes(COPY.heading),
				previewDisabled: await previewButton(page).isDisabled()
			}),
			'NZBP_CANCEL',
			FIX_PAGE
		);
		await expectCleanRun(page, nzbp, 'NZBP-R6-06');
	});
});

test.describe('NZBP interaction contract', () => {
	test('NZBP-UX-01 Preview enables only for a valid selection', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		const engine = await engineOf(page);
		await receipt(
			info,
			engine,
			'NZBP-UX-01',
			{ idleDisabled: true, invalidDisabled: true, validEnabled: true },
			async () => {
				const idleDisabled = await previewButton(page).isDisabled({ timeout: 3000 });
				await page.getByTestId('import-file').setInputFiles({ name: 'empty.nzb', mimeType: 'application/x-nzb', buffer: Buffer.alloc(0) });
				await expect(page.getByTestId('import-status')).toHaveAttribute('role', 'alert');
				const invalidDisabled = await previewButton(page).isDisabled({ timeout: 3000 });
				await selectNzb(page, 'ux-01.nzb', nzbXml(fileXml([segmentXml({ bytes: '10', number: '1', text: 'mid' })])));
				const validEnabled = await previewButton(page).isEnabled({ timeout: 3000 });
				return { idleDisabled, invalidDisabled, validEnabled };
			},
			'NZBP_GESTURE',
			FIX_PAGE
		);
		await expectCleanRun(page, nzbp, 'NZBP-UX-01');
	});

	test('NZBP-UX-02 keyboard order keeps focus through the busy swap', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		await selectNzb(page, 'ux-02.nzb', largeValidXml(5000));
		const read = await holdNextStreamRead(page);
		const engine = await engineOf(page);
		await receipt(
			info,
			engine,
			'NZBP-UX-02',
			{ afterFirstTab: 'Preview', afterSecondTab: 'Clear selection', busyFocus: 'Cancel', activation: 'keyboard' },
			async () => {
				await page.getByTestId('import-file').focus();
				await page.keyboard.press('Tab');
				const afterFirstTab = await page.evaluate(() => document.activeElement?.textContent?.trim() ?? '');
				await page.keyboard.press('Tab');
				const afterSecondTab = await page.evaluate(() => document.activeElement?.textContent?.trim() ?? '');
				await page.keyboard.press('Shift+Tab');
				await page.keyboard.press('Enter');
				await read.waitUntilReading();
				await expect.poll(() => page.evaluate(() => document.activeElement?.textContent?.trim() ?? ''), 'NZBP-UX-02: focus lands on Cancel').toBe('Cancel');
				const busyFocus = await page.evaluate(() => document.activeElement?.textContent?.trim() ?? '');
				await cancelButton(page).click();
				await read.release();
				expect(await settled(page), 'NZBP-UX-02: cancellation survives the released read').toEqual({ status: 'cancelled' });
				return { afterFirstTab, afterSecondTab, busyFocus, activation: 'keyboard' as const };
			},
			'NZBP_FOCUS',
			FIX_PAGE
		);
		await expectCleanRun(page, nzbp, 'NZBP-UX-02');
	});

	test('NZBP-UX-03 input description extends to the preview region', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		await selectNzb(page, 'ux-03.nzb', ax01Xml());
		await (await requirePreview(page, 'NZBP-UX-03')).click();
		await settled(page);
		await receipt(
			info,
			await engineOf(page),
			'NZBP-UX-03',
			{ describedByPreview: true },
			async () => ({
				describedByPreview: await page.evaluate((heading) => {
					const input = document.querySelector('[data-testid="import-file"]');
					const ids = (input?.getAttribute('aria-describedby') ?? '').split(/\s+/).filter(Boolean);
					return ids.some((id) => document.getElementById(id)?.textContent?.includes(heading) ?? false);
				}, COPY.heading)
			}),
			'NZBP_FOCUS',
			FIX_PAGE
		);
		await expectCleanRun(page, nzbp, 'NZBP-UX-03');
	});

	test('NZBP-UX-05 Clear removes summary, error and probe together', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		await selectNzb(page, 'ux-05.nzb', ax01Xml());
		await (await requirePreview(page, 'NZBP-UX-05')).click();
		await settled(page);
		await page.getByRole('button', { name: 'Clear selection', exact: true }).click();
		await receipt(
			info,
			await engineOf(page),
			'NZBP-UX-05',
			{ outcome: null, summaryAbsent: true, feedbackGone: true },
			async () => ({
				outcome: (await probe(page)) ?? null,
				summaryAbsent: !(await page.getByRole('main').innerText()).includes(COPY.heading),
				feedbackGone: (await page.getByTestId('import-status').count()) === 0
			}),
			'NZBP_CANCEL',
			FIX_PAGE
		);
		await expectCleanRun(page, nzbp, 'NZBP-UX-05');
	});

	test('NZBP-UX-06 re-preview repeats on a new generation', async ({ page, baseURL, nzbp }, info) => {
		await openImport(page, baseURL);
		await selectNzb(page, 'ux-06.nzb', largeValidXml(5000));
		await (await requirePreview(page, 'NZBP-UX-06')).click();
		const first = await settled(page);
		const read = await holdNextStreamRead(page);
		await (await requirePreview(page, 'NZBP-UX-06')).click();
		await read.waitUntilReading();
		await expect(cancelButton(page), 'NZBP-UX-06: second read starts').toBeVisible({ timeout: 5000 });
		await read.release();
		const second = await settled(page);
		await receipt(info, await engineOf(page), 'NZBP-UX-06', { repeated: true }, async () => ({ repeated: JSON.stringify(first) === JSON.stringify(second) && (second as { status?: unknown }).status === 'ok' }), 'NZBP_GESTURE', FIX_PAGE);
		await expectCleanRun(page, nzbp, 'NZBP-UX-06');
	});
});

for (const scheme of ['light', 'dark'] as const) {
	test.describe(`NZBP narrow ${scheme}`, () => {
		test.use({ viewport: { width: 360, height: 800 }, colorScheme: scheme });
		test(`NZBP-UX-04 narrow ${scheme} renders without sideways scroll`, async ({ page, baseURL, nzbp }, info) => {
			await openImport(page, baseURL);
			await selectNzb(page, `ux-04-${scheme}.nzb`, ax01Xml());
			await (await requirePreview(page, 'NZBP-UX-04')).click();
			await settled(page);
			await receipt(
				info,
				await engineOf(page),
				`NZBP-UX-04-${scheme}`,
				{ headingVisible: true, fitsWidth: true, previewTallEnough: true, clearTallEnough: true },
				async () => {
					const heading = page.getByRole('heading', { name: COPY.heading, exact: true });
					const previewBox = await previewButton(page).boundingBox();
					const clearBox = await page.getByRole('button', { name: 'Clear selection', exact: true }).boundingBox();
					return {
						headingVisible: await heading.isVisible(),
						fitsWidth: await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1),
						previewTallEnough: (previewBox?.height ?? 0) >= 44,
						clearTallEnough: (clearBox?.height ?? 0) >= 44
					};
				},
				'NZBP_LAYOUT',
				FIX_PAGE
			);
			await expectCleanRun(page, nzbp, `NZBP-UX-04-${scheme}`);
		});
	});
}

test.describe('NZBP fixed parser budgets', () => {
	const PARSE_BUDGET_MS = 500;
	const TOTAL_BUDGET_MS = 1000;
	const REPETITIONS = 5;

	function hostileShapes(): [string, () => string][] {
		return [
			['deep-nesting', () => nzbXml(`${'<wrap>'.repeat(63)}<leaf/>${'</wrap>'.repeat(63)}`)],
			[
				'element-density',
				() => nzbXml(`${'<m/>'.repeat(60000)}${'<!-- density-comment -->'.repeat(40000)}`)
			],
			['near-cap-attribute', () => nzbXml(`<meta note="${'a'.repeat(1900000)}"/>`)],
			['near-cap-text', () => nzbXml(fileXml([segmentXml({ bytes: '1', number: '1', text: 't'.repeat(1900000) })]))],
			['reference-dense', () => nzbXml(fileXml([segmentXml({ bytes: '1', number: '1', text: '&#65;'.repeat(100000) })]))],
			['sixty-k-segments', () => nzbXml(fileXml('<segment bytes="1">x</segment>'.repeat(60000), 'r2-entry'))]
		];
	}

	for (const [shape, build] of hostileShapes()) {
		test(`NZBP-R2-${shape} finishes inside the fixed budgets`, async ({ page, baseURL, nzbp }, info) => {
			test.slow();
			await openImport(page, baseURL);
			const engine = await engineOf(page);
			const xml = build();
			expect(Buffer.byteLength(xml), `NZBP-R2-${shape}: hostile shape stays under the read cap`).toBeLessThanOrEqual(2 * 1024 * 1024);
			const reps: { parse_ms: number; total_ms: number; parses: number }[] = [];
			for (let rep = 0; rep < REPETITIONS; rep += 1) {
				await selectNzb(page, `r2-${shape}-r${rep}.nzb`, xml);
				expect(await probe(page), `NZBP-R2-${shape}: probe resets on replacement`).toBeNull();
				const parsesBefore = (await parseDurations(page)).length;
				const started = Date.now();
				await (await requirePreview(page, `NZBP-R2-${shape}`)).click();
				await settled(page);
				const totalMs = Date.now() - started;
				const fresh = (await parseDurations(page)).slice(parsesBefore);
				reps.push({ parse_ms: fresh.length === 1 ? Math.round(fresh[0]! * 100) / 100 : -1, total_ms: totalMs, parses: fresh.length });
			}
			await receipt(
				info,
				engine,
				`NZBP-R2-${shape}`,
				{
					engine,
					repetitions: REPETITIONS,
					parse_budget_ms: PARSE_BUDGET_MS,
					total_budget_ms: TOTAL_BUDGET_MS,
					withinBudget: true,
					singleParse: true
				},
				async () => ({
					engine,
					repetitions: reps.length,
					parse_budget_ms: PARSE_BUDGET_MS,
					total_budget_ms: TOTAL_BUDGET_MS,
					withinBudget: reps.every((rep) => rep.parse_ms >= 0 && rep.parse_ms <= PARSE_BUDGET_MS && rep.total_ms <= TOTAL_BUDGET_MS),
					singleParse: reps.every((rep) => rep.parses === 1)
				}),
				'NZBP_BUDGET',
				'Return the over-budget shape to the security owner; do not raise the budget.'
			);
			console.log(JSON.stringify({ bank: 'playwright', case_id: `NZBP-R2-${shape}-timings`, engine, reps }));
			await expectCleanRun(page, nzbp, `NZBP-R2-${shape}`);
		});
	}
});
