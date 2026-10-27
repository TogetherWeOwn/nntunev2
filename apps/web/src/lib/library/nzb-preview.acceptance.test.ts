import { describe, expect, it } from 'vitest';

import { MAX_NZB_IMPORT_BYTES } from './import-validation.js';
import packageRaw from '../../../package.json?raw';
import nzbIntakeRaw from '../../../../../workers/ingest/src/nzb_intake.rs?raw';

/**
 * NZBP — protected acceptance bank, fast half (no browser): bounded local NZB
 * claims preview. The browser half is `tests/nzb-preview.acceptance.spec.ts`.
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build implements the
 * contract below on top of this branch and must make these tests pass WITHOUT
 * editing this file; a wrong test comes back to the acceptance owner instead.
 *
 * LOCKED BUILDER CONTRACT (pure core; all DOM-free):
 * - Module `src/lib/library/nzb-preview.ts` (this directory). It must not
 *   reference `DOMParser`, `window.*` or `document.*`: parsing against the
 *   real browser parser lives in the import page (or a preview component
 *   whose filename contains `nzb-preview`), which adapts live `Element`
 *   nodes into `NzbpElement` trees and calls `summarizeNzbElements`.
 * - `NZBP_PREVIEW_CONFIG_VERSION: number` (= 1, mirrors the server intake
 *   config version), `NZBP_NZB_NAMESPACE: string` (the newzbin namespace),
 *   `NZBP_LIMITS` (frozen object with numeric `maxBytes`, `maxFiles`,
 *   `maxSegments`, `maxDeclaredBytes`, `maxDepth`, `maxElements`,
 *   `maxAttributesPerElement`, `maxAttributeValueChars`,
 *   `maxSegmentTextChars`, `xmlDeclarationScanBytes`).
 * - `prescanNzbProlog(text): { ok: true; documentText: string } |
 *   { ok: false; errorCode: string }` — fail-closed prolog grammar over the
 *   decoded string: skip BOM/whitespace/comments/PIs as whole units, allow
 *   zero or one exact-case `<!DOCTYPE` followed by whitespace, excise a
 *   subset-free DOCTYPE by replacing it with nothing, reject `[` outside
 *   quotes / unterminated quotes / a second `<!DOCTYPE` / any `<!ENTITY`
 *   anywhere with `NZBP_DOCTYPE_NOT_ALLOWED`, enforce the XML-declaration
 *   policy (`version="1.0"`, UTF-8 or absent, closed in the scan window).
 *   Never throws for hostile input; ambiguous shapes fail, never guessed.
 * - `summarizeNzbElements(root: NzbpElement): NzbpOutcome` — iterative
 *   counter over the minimal adapter `{ localName, namespaceURI,
 *   attributes: [{ name, value }] (qualified names), childElements,
 *   textContent }` (concatenated descendant text, DOM semantics). Counting
 *   is by case-sensitive local name anywhere in the document (prefixes and
 *   descendant namespaces ignored, matching the server intake); only the
 *   root's namespace is checked (absent or newzbin, else `NZBP_NOT_NZB`).
 *   Only unprefixed `bytes`/`number` attributes are interpreted. Segment
 *   identity is `textContent` trimmed. Root depth is 1; deeper than 16
 *   fails. Duplicate counters count occurrences beyond the first (a pair
 *   counts 1; numbers scoped per file entry, message-ids global).
 * - Outcomes: `{ status: 'ok', error_code: null, next_action: 'none',
 *   summary }` with exactly the 11 documented summary keys
 *   (`preview_config_version`, `file_entry_count`, `segment_count`,
 *   `declared_bytes_total` as a decimal string or null,
 *   `declared_bytes_basis` one of `all_segments_claim_bytes` / `unknown` /
 *   `no_segments`, `segments_without_byte_claim`, `segments_without_number`,
 *   `duplicate_segment_numbers`, `duplicate_message_ids`,
 *   `files_without_segments`, `files_with_multiple_segment_groups`); or
 *   `{ status: 'error', error_code, next_action }` with no `summary` key.
 *   Check order: actual read size (page) -> depth/element caps ->
 *   per-element attribute count/value-length caps (UTF-16 code units, every
 *   attribute, before any value is interpreted) -> digit test
 *   (`/^[0-9]+$/`; anything else is a missing claim, never zero) -> BigInt
 *   per-value cap -> BigInt running-sum cap. File/segment count caps sit
 *   with the structural caps, before numeric checks.
 *
 * Case IDs: `NZBP-AX-01..16` mirror the design table (byte/read-path halves
 * live in the browser bank, marked below); `NZBP-BOUND-*` pin at-cap
 * acceptance; `NZBP-R1/R3/R4/R5-*` pin the security conditions;
 * `NZBP-DRIFT-*` pin cap parity with the server intake source;
 * `NZBP-SRC-*` pin page/cmponent mechanisms that no honest browser fixture
 * can distinguish; `NZBP-CTRL-*` are controls that stay GREEN on the
 * unmodified base and prove the harness (raw imports, parsers) works.
 * Every case logs one compact JSON line:
 * `bank/case_id/expected/actual/status/error_code/next_action`.
 *
 * Deliberate gaps (builder latitude, both resolutions safe): a `<!DOCTYPE`
 * after the root element is unpinned (reject or let the parser fail).
 *
 * Focused RED/GREEN (offline, deterministic):
 *   pnpm --dir apps/web exec vitest run src/lib/library/nzb-preview.acceptance.test.ts
 * Type gate (must stay green — these files ship in the final PR):
 *   pnpm --dir apps/web check
 */

type NzbpTestAttribute = { name: string; value: string };
type NzbpTestElement = {
	localName: string;
	namespaceURI: string | null;
	attributes: NzbpTestAttribute[];
	childElements: NzbpTestElement[];
	textContent: string;
};

type NzbpModule = {
	NZBP_PREVIEW_CONFIG_VERSION?: unknown;
	NZBP_NZB_NAMESPACE?: unknown;
	NZBP_LIMITS?: unknown;
	prescanNzbProlog?: (text: string) => unknown;
	summarizeNzbElements?: (root: NzbpTestElement) => unknown;
};

// Missing module/export is an ASSERTION failure (feature RED), never a
// collection error: the glob simply yields no entry on the unmodified base.
const previewModules = import.meta.glob<NzbpModule>('./nzb-preview.ts', { eager: true });
const previewModule = previewModules['./nzb-preview.ts'];

const previewModuleRawModules = import.meta.glob<string>('./nzb-preview.ts', {
	query: '?raw',
	import: 'default',
	eager: true
});
const previewModuleRaw = previewModuleRawModules['./nzb-preview.ts'];

const routeSources = import.meta.glob<string>('../../routes/**/*.svelte', {
	query: '?raw',
	import: 'default',
	eager: true
});
const libComponentSources = import.meta.glob<string>('../**/*.svelte', {
	query: '?raw',
	import: 'default',
	eager: true
});
const apiRouteKeys = Object.keys(import.meta.glob('../../routes/api/**/*'));

const importRouteRaw = routeSources['../../routes/library/import/+page.svelte'] ?? '';
const previewComponentRaw = Object.entries(libComponentSources)
	.filter(([path]) => path.includes('nzb-preview'))
	.map(([, source]) => source)
	.join('\n');
const previewUiRaw = `${importRouteRaw}\n${previewComponentRaw}`;
const previewCoreRaw = previewModuleRaw ?? '';

// Synthetic-only markers. Never real subjects, ids, posters or URLs; the
// `.invalid` host never resolves (and the browser bank blocks all traffic).
const SENTINELS = {
	subject: 'NZBP-SENTINEL-SUBJECT-7f3a',
	poster: 'NZBP-SENTINEL-POSTER-9c1e',
	group: 'NZBP-SENTINEL-GROUP-4b77',
	mid: 'NZBP-SENTINEL-MID-2d8f',
	dtd: 'NZBP-SENTINEL-DTD-55f0',
	malformed: 'NZBP-SENTINEL-MALFORMED-aa92'
} as const;

const NZB_NAMESPACE = 'http://www.newzbin.com/DTD/2003/nzb';
const SIXTY_FOUR_GIB = '68719476736';

function el(
	localName: string,
	options: {
		attrs?: [string, string][];
		children?: NzbpTestElement[];
		text?: string;
		ns?: string | null;
	} = {}
): NzbpTestElement {
	return {
		localName,
		namespaceURI: options.ns ?? null,
		attributes: (options.attrs ?? []).map(([name, value]) => ({ name, value })),
		childElements: options.children ?? [],
		textContent: options.text ?? ''
	};
}

function segment(options: { bytes?: string; number?: string; text?: string; extra?: [string, string][] } = {}): NzbpTestElement {
	const attrs: [string, string][] = [...(options.extra ?? [])];
	if (options.bytes !== undefined) attrs.push(['bytes', options.bytes]);
	if (options.number !== undefined) attrs.push(['number', options.number]);
	return el('segment', { attrs, text: options.text ?? `${SENTINELS.mid}-default` });
}

function fileEntry(segments: NzbpTestElement[], fileAttrs: [string, string][] = [['subject', SENTINELS.subject]]): NzbpTestElement {
	return el('file', { attrs: fileAttrs, children: [el('segments', { children: segments })] });
}

function nzbDoc(children: NzbpTestElement[], rootAttrs: [string, string][] = []): NzbpTestElement {
	return el('nzb', { attrs: rootAttrs, children });
}

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

const okOutcome = (summaryValue: Record<string, unknown>) => ({
	status: 'ok',
	error_code: null,
	next_action: 'none',
	summary: summaryValue
});
const errOutcome = (code: string, action = 'choose_different_file') => ({
	status: 'error',
	error_code: code,
	next_action: action
});

function callPrescan(text: string): unknown {
	if (typeof previewModule?.prescanNzbProlog !== 'function') {
		return { error: 'preview export missing: prescanNzbProlog' };
	}
	try {
		return previewModule.prescanNzbProlog(text);
	} catch (error) {
		return { error: `prescan threw: ${error instanceof Error ? error.message : String(error)}` };
	}
}

function callSummarize(root: NzbpTestElement): unknown {
	if (typeof previewModule?.summarizeNzbElements !== 'function') {
		return { error: 'preview export missing: summarizeNzbElements' };
	}
	try {
		return previewModule.summarizeNzbElements(root);
	} catch (error) {
		return { error: `summarize threw: ${error instanceof Error ? error.message : String(error)}` };
	}
}

function checkCase(
	id: string,
	expected: unknown,
	actual: unknown,
	failCode: string,
	fix: string,
	options: { sentinels?: string[]; anyOfCodes?: string[] } = {}
): void {
	const sentinels = options.sentinels ?? [];
	let mismatch = '';
	try {
		if (options.anyOfCodes) {
			const shape = actual as { ok?: unknown; errorCode?: unknown };
			expect(shape?.ok).toBe(false);
			expect(options.anyOfCodes).toContain(shape?.errorCode);
			expect(Object.keys(shape ?? {}).sort()).toEqual(['errorCode', 'ok']);
		} else {
			expect(actual).toEqual(expected);
		}
	} catch (error) {
		mismatch = error instanceof Error ? (error.message.split('\n')[0] ?? 'mismatch') : String(error);
	}
	const echoed = sentinels.filter((marker) => JSON.stringify(actual ?? null).includes(marker));
	const passed = mismatch === '' && echoed.length === 0;
	console.log(
		JSON.stringify({
			bank: 'vitest',
			case_id: id,
			expected,
			actual,
			status: passed ? 'PASS' : 'FAIL',
			error_code: passed ? null : echoed.length > 0 ? 'NZBP_NO_ECHO' : failCode,
			next_action: passed ? 'none' : fix
		})
	);
	if (!passed) {
		if (echoed.length > 0) {
			expect(echoed, `${id}: file-content strings echoed in the outcome`).toEqual([]);
		} else if (options.anyOfCodes) {
			const shape = actual as { ok?: unknown; errorCode?: unknown };
			expect(shape?.ok, `${id}: prescan must fail closed`).toBe(false);
			expect(options.anyOfCodes, `${id}: unexpected error code`).toContain(shape?.errorCode);
		} else {
			expect(actual, `${id}: ${mismatch}`).toEqual(expected);
		}
	}
}

const FIX_CORE = 'Implement the pure preview core; preserve this protected bank.';

describe('NZBP contract presence', () => {
	it('NZBP-CTR-01 prescan export exists', () => {
		checkCase(
			'NZBP-CTR-01',
			{ prescan: 'function' },
			{ prescan: typeof previewModule?.prescanNzbProlog === 'function' ? 'function' : 'missing' },
			'NZBP_CONTRACT',
			FIX_CORE
		);
	});

	it('NZBP-CTR-02 summarize export exists', () => {
		checkCase(
			'NZBP-CTR-02',
			{ summarize: 'function' },
			{ summarize: typeof previewModule?.summarizeNzbElements === 'function' ? 'function' : 'missing' },
			'NZBP_CONTRACT',
			FIX_CORE
		);
	});

	it('NZBP-CTR-03 limits, version and namespace exports exist', () => {
		const limits = previewModule?.NZBP_LIMITS as Record<string, unknown> | undefined;
		checkCase(
			'NZBP-CTR-03',
			{
				limits: 'object',
				version: 'number',
				namespace: 'string',
				limitKeys: [
					'maxAttributeValueChars',
					'maxAttributesPerElement',
					'maxBytes',
					'maxDeclaredBytes',
					'maxDepth',
					'maxElements',
					'maxFiles',
					'maxSegmentTextChars',
					'maxSegments',
					'xmlDeclarationScanBytes'
				]
			},
			{
				limits: limits && typeof limits === 'object' ? 'object' : 'missing',
				version: typeof previewModule?.NZBP_PREVIEW_CONFIG_VERSION === 'number' ? 'number' : 'missing',
				namespace: typeof previewModule?.NZBP_NZB_NAMESPACE === 'string' ? 'string' : 'missing',
				limitKeys: limits && typeof limits === 'object' ? Object.keys(limits).sort() : []
			},
			'NZBP_CONTRACT',
			FIX_CORE
		);
	});

	it('NZBP-CTR-04 preview module source exists and is DOM-free', () => {
		const exists = previewModuleRaw !== undefined;
		checkCase(
			'NZBP-CTR-04',
			{ exists: true, domFree: true },
			{
				exists,
				domFree:
					exists &&
					!/\bDOMParser\b/.test(previewCoreRaw) &&
					!/\bwindow\./.test(previewCoreRaw) &&
					!/\bdocument\./.test(previewCoreRaw)
			},
			'NZBP_CONTRACT',
			FIX_CORE
		);
	});
});

describe('NZBP-AX core outcomes', () => {
	it('NZBP-AX-01 minimal valid document counts and sums', () => {
		const root = nzbDoc([
			fileEntry([segment({ bytes: '100', number: '1', text: `${SENTINELS.mid}-a` }), segment({ bytes: '200', number: '2', text: `${SENTINELS.mid}-b` })])
		]);
		checkCase(
			'NZBP-AX-01',
			okOutcome(
				summary({
					file_entry_count: 1,
					segment_count: 2,
					declared_bytes_total: '300',
					declared_bytes_basis: 'all_segments_claim_bytes'
				})
			),
			callSummarize(root),
			'NZBP_SUMMARY',
			FIX_CORE,
			{ sentinels: [SENTINELS.subject, `${SENTINELS.mid}-a`, `${SENTINELS.mid}-b`] }
		);
	});

	it('NZBP-AX-02 one segment without bytes makes the total unknown', () => {
		const root = nzbDoc([fileEntry([segment({ bytes: '100', number: '1', text: 'mid-known' }), segment({ number: '2', text: 'mid-unknown' })])]);
		checkCase(
			'NZBP-AX-02',
			okOutcome(
				summary({
					file_entry_count: 1,
					segment_count: 2,
					declared_bytes_total: null,
					declared_bytes_basis: 'unknown',
					segments_without_byte_claim: 1
				})
			),
			callSummarize(root),
			'NZBP_SUMMARY',
			FIX_CORE,
			{ sentinels: ['mid-known', 'mid-unknown'] }
		);
	});

	it('NZBP-AX-03 zero file entries yields no-segments basis, not zero bytes', () => {
		checkCase('NZBP-AX-03', okOutcome(summary()), callSummarize(nzbDoc([])), 'NZBP_SUMMARY', FIX_CORE);
	});

	it('NZBP-AX-06 internal subset with entities is rejected', () => {
		checkCase(
			'NZBP-AX-06',
			{ ok: false, errorCode: 'NZBP_DOCTYPE_NOT_ALLOWED' },
			callPrescan('<?xml version="1.0"?><!DOCTYPE nzb [<!ENTITY a "x"><!ENTITY b "&a;&a;">]><nzb/>'),
			'NZBP_PRESCAN',
			FIX_CORE
		);
	});

	it('NZBP-AX-07 bare PUBLIC doctype is excised, never fetched', () => {
		const systemId = `http://${SENTINELS.dtd}.invalid/nzb-1.1.dtd`;
		const input = `<?xml version="1.0"?>\n<!DOCTYPE nzb PUBLIC "-//nzbp//DTD NZB 1.1//EN" "${systemId}">\n<nzb></nzb>`;
		checkCase(
			'NZBP-AX-07',
			{ ok: true, documentText: '<?xml version="1.0"?>\n\n<nzb></nzb>' },
			callPrescan(input),
			'NZBP_PRESCAN',
			FIX_CORE,
			{ sentinels: [systemId, '-//nzbp//DTD NZB 1.1//EN', '<!DOCTYPE'] }
		);
	});

	it('NZBP-AX-08 depth 17 fails', () => {
		let deep: NzbpTestElement = el('leaf');
		for (let depth = 0; depth < 16; depth += 1) deep = el('wrap', { children: [deep] });
		checkCase('NZBP-AX-08', errOutcome('NZBP_TOO_DEEP'), callSummarize(nzbDoc([deep])), 'NZBP_CAPS', FIX_CORE);
	});

	it('NZBP-AX-09 all-digit over-cap bytes value fails', () => {
		const root = nzbDoc([fileEntry([segment({ bytes: '99999999999999999999', number: '1', text: 'mid-big' })])]);
		checkCase('NZBP-AX-09', errOutcome('NZBP_DECLARED_BYTES_OVER_CAP'), callSummarize(root), 'NZBP_NUMERIC', FIX_CORE, {
			sentinels: ['mid-big', '99999999999999999999']
		});
	});

	it('NZBP-AX-10 duplicate number and message-id count 1 each, ids absent', () => {
		const root = nzbDoc([
			fileEntry([segment({ bytes: '10', number: '7', text: 'dup-mid-1' }), segment({ bytes: '10', number: '7', text: 'other-mid' })]),
			fileEntry([segment({ bytes: '10', number: '1', text: 'dup-mid-1' })])
		]);
		checkCase(
			'NZBP-AX-10',
			okOutcome(
				summary({
					file_entry_count: 2,
					segment_count: 3,
					declared_bytes_total: '30',
					declared_bytes_basis: 'all_segments_claim_bytes',
					duplicate_segment_numbers: 1,
					duplicate_message_ids: 1
				})
			),
			callSummarize(root),
			'NZBP_SUMMARY',
			FIX_CORE,
			{ sentinels: ['dup-mid-1', 'other-mid'] }
		);
	});

	it('NZBP-AX-12 prescan passes malformed markup through untouched (parser fails later)', () => {
		// The pre-scan reads tokens only; tag balance is the parser's job in
		// the browser bank. Exact passthrough (no echo rule applies to this
		// intermediate string — only to outcome JSON and rendered text).
		const input = `<nzb><file subject="${SENTINELS.malformed}"><segments><segment bytes="10">x</seg></segment></segments></file></nzb>`;
		checkCase('NZBP-AX-12', { ok: true, documentText: input }, callPrescan(input), 'NZBP_PRESCAN', FIX_CORE);
	});

	it('NZBP-AX-15 non-nzb root and foreign namespace both fail', () => {
		const cases = [
			{ name: 'non_nzb_root', root: el('rss', { children: [] }) },
			{ name: 'foreign_ns', root: el('nzb', { ns: 'http://foreign.invalid/nzb' }) }
		];
		const actual: Record<string, unknown> = {};
		for (const entry of cases) actual[entry.name] = callSummarize(entry.root);
		checkCase(
			'NZBP-AX-15',
			{ non_nzb_root: errOutcome('NZBP_NOT_NZB'), foreign_ns: errOutcome('NZBP_NOT_NZB') },
			actual,
			'NZBP_NOT_NZB',
			FIX_CORE
		);
	});

	it('NZBP-AX-16 segment cap accepts 60000 and rejects 60001', () => {
		const build = (count: number) =>
			nzbDoc([fileEntry(Array.from({ length: count }, (_, index) => segment({ bytes: '1', number: `${index + 1}`, text: `mid-${index}` })), [['subject', 'bulk']])]);
		const actual = { at_cap: callSummarize(build(60000)), over_cap: callSummarize(build(60001)) };
		checkCase(
			'NZBP-AX-16',
			{
				at_cap: okOutcome(
					summary({ file_entry_count: 1, segment_count: 60000, declared_bytes_total: '60000', declared_bytes_basis: 'all_segments_claim_bytes' })
				),
				over_cap: errOutcome('NZBP_TOO_MANY_SEGMENTS')
			},
			actual,
			'NZBP_CAPS',
			FIX_CORE,
			{ sentinels: ['mid-0', 'mid-59999'] }
		);
	});
});

describe('NZBP at-cap boundaries', () => {
	it('NZBP-BOUND-01 depth 16 is accepted', () => {
		let chain: NzbpTestElement = el('leaf');
		for (let depth = 0; depth < 14; depth += 1) chain = el('wrap', { children: [chain] });
		const actual = callSummarize(nzbDoc([chain]));
		checkCase('NZBP-BOUND-01', okOutcome(summary()), actual, 'NZBP_CAPS', FIX_CORE);
	});

	it('NZBP-BOUND-02 exactly 150000 elements are accepted', () => {
		const children = Array.from({ length: 149999 }, () => el('meta'));
		checkCase('NZBP-BOUND-02', okOutcome(summary()), callSummarize(nzbDoc(children)), 'NZBP_CAPS', FIX_CORE);
	});

	it('NZBP-BOUND-03 150001 elements fail', () => {
		const children = Array.from({ length: 150000 }, () => el('meta'));
		checkCase('NZBP-BOUND-03', errOutcome('NZBP_TOO_MANY_NODES'), callSummarize(nzbDoc(children)), 'NZBP_CAPS', FIX_CORE);
	});

	it('NZBP-BOUND-04 16 attributes on one element are accepted', () => {
		const attrs = Array.from({ length: 16 }, (_, index) => [`k${index}`, 'v'] as [string, string]);
		checkCase('NZBP-BOUND-04', okOutcome(summary({ file_entry_count: 0 })), callSummarize(nzbDoc([el('meta', { attrs })])), 'NZBP_CAPS', FIX_CORE);
	});

	it('NZBP-BOUND-05 17 attributes on one element fail', () => {
		const attrs = Array.from({ length: 17 }, (_, index) => [`k${index}`, 'v'] as [string, string]);
		checkCase('NZBP-BOUND-05', errOutcome('NZBP_TOO_MANY_NODES'), callSummarize(nzbDoc([el('meta', { attrs })])), 'NZBP_CAPS', FIX_CORE);
	});

	it('NZBP-BOUND-06 1024-char attribute value is accepted', () => {
		const root = nzbDoc([fileEntry([segment({ bytes: '5', number: '1', text: 'mid-ok', extra: [['note', 'n'.repeat(1024)]] })])]);
		checkCase(
			'NZBP-BOUND-06',
			okOutcome(summary({ file_entry_count: 1, segment_count: 1, declared_bytes_total: '5', declared_bytes_basis: 'all_segments_claim_bytes' })),
			callSummarize(root),
			'NZBP_CAPS',
			FIX_CORE,
			{ sentinels: ['mid-ok', 'n'.repeat(64)] }
		);
	});

	it('NZBP-BOUND-08 512-char segment text is accepted', () => {
		const root = nzbDoc([fileEntry([segment({ bytes: '5', number: '1', text: 't'.repeat(512) })])]);
		checkCase(
			'NZBP-BOUND-08',
			okOutcome(summary({ file_entry_count: 1, segment_count: 1, declared_bytes_total: '5', declared_bytes_basis: 'all_segments_claim_bytes' })),
			callSummarize(root),
			'NZBP_CAPS',
			FIX_CORE,
			{ sentinels: ['t'.repeat(64)] }
		);
	});

	it('NZBP-BOUND-09 513-char segment text fails', () => {
		const root = nzbDoc([fileEntry([segment({ bytes: '5', number: '1', text: 't'.repeat(513) })])]);
		checkCase('NZBP-BOUND-09', errOutcome('NZBP_TOO_MANY_NODES'), callSummarize(root), 'NZBP_CAPS', FIX_CORE, {
			sentinels: ['t'.repeat(64)]
		});
	});

	it('NZBP-BOUND-10 5000 file entries are accepted', () => {
		const files = Array.from({ length: 5000 }, () => el('file'));
		checkCase(
			'NZBP-BOUND-10',
			okOutcome(summary({ file_entry_count: 5000, files_without_segments: 5000 })),
			callSummarize(nzbDoc(files)),
			'NZBP_CAPS',
			FIX_CORE
		);
	});

	it('NZBP-BOUND-11 5001 file entries fail', () => {
		const files = Array.from({ length: 5001 }, () => el('file'));
		checkCase('NZBP-BOUND-11', errOutcome('NZBP_TOO_MANY_FILES'), callSummarize(nzbDoc(files)), 'NZBP_CAPS', FIX_CORE);
	});

	it('NZBP-BOUND-12 per-value 64 GiB exactly is accepted', () => {
		const root = nzbDoc([fileEntry([segment({ bytes: SIXTY_FOUR_GIB, number: '1', text: 'mid-cap' })])]);
		checkCase(
			'NZBP-BOUND-12',
			okOutcome(summary({ file_entry_count: 1, segment_count: 1, declared_bytes_total: SIXTY_FOUR_GIB, declared_bytes_basis: 'all_segments_claim_bytes' })),
			callSummarize(root),
			'NZBP_NUMERIC',
			FIX_CORE,
			{ sentinels: ['mid-cap'] }
		);
	});

	it('NZBP-BOUND-13 per-value 64 GiB + 1 fails', () => {
		const root = nzbDoc([fileEntry([segment({ bytes: '68719476737', number: '1', text: 'mid-over' })])]);
		checkCase('NZBP-BOUND-13', errOutcome('NZBP_DECLARED_BYTES_OVER_CAP'), callSummarize(root), 'NZBP_NUMERIC', FIX_CORE, {
			sentinels: ['mid-over', '68719476737']
		});
	});

	it('NZBP-BOUND-14 running sum over 64 GiB fails', () => {
		const root = nzbDoc([fileEntry([segment({ bytes: SIXTY_FOUR_GIB, number: '1', text: 'mid-a' }), segment({ bytes: '1', number: '2', text: 'mid-b' })])]);
		checkCase('NZBP-BOUND-14', errOutcome('NZBP_DECLARED_BYTES_OVER_CAP'), callSummarize(root), 'NZBP_NUMERIC', FIX_CORE, {
			sentinels: ['mid-a', 'mid-b']
		});
	});
});

describe('NZBP-R1 fail-closed prolog grammar', () => {
	const R1 = 'NZBP_PRESCAN';
	it('NZBP-R1-01 doctype inside a comment fails, never excised', () => {
		checkCase('NZBP-R1-01', { ok: false, errorCode: ['NZBP_DOCTYPE_NOT_ALLOWED', 'NZBP_MALFORMED_XML'] }, callPrescan('<!-- <!DOCTYPE nzb [<!ENTITY x "y">]> --><nzb/>'), R1, FIX_CORE, {
			anyOfCodes: ['NZBP_DOCTYPE_NOT_ALLOWED', 'NZBP_MALFORMED_XML']
		});
	});

	it('NZBP-R1-02 doctype inside a processing instruction fails', () => {
		checkCase('NZBP-R1-02', { ok: false, errorCode: ['NZBP_DOCTYPE_NOT_ALLOWED', 'NZBP_MALFORMED_XML'] }, callPrescan('<?pi <!DOCTYPE nzb [<!ENTITY x "y">]> ?><nzb/>'), R1, FIX_CORE, {
			anyOfCodes: ['NZBP_DOCTYPE_NOT_ALLOWED', 'NZBP_MALFORMED_XML']
		});
	});

	it('NZBP-R1-03 a second doctype fails', () => {
		checkCase('NZBP-R1-03', { ok: false, errorCode: 'NZBP_DOCTYPE_NOT_ALLOWED' }, callPrescan('<!DOCTYPE nzb><!DOCTYPE nzb><nzb/>'), R1, FIX_CORE);
	});

	it('NZBP-R1-04 bracket and close inside a quoted PUBLIC id are not syntax', () => {
		checkCase('NZBP-R1-04', { ok: true, documentText: '<nzb/>' }, callPrescan('<!DOCTYPE nzb PUBLIC "a>b[c" "x.dtd"><nzb/>'), R1, FIX_CORE, {
			sentinels: ['<!DOCTYPE']
		});
	});

	it('NZBP-R1-05 bracket and close inside a quoted SYSTEM id are not syntax', () => {
		checkCase('NZBP-R1-05', { ok: true, documentText: '<nzb/>' }, callPrescan('<!DOCTYPE nzb SYSTEM "a>b[c.dtd"><nzb/>'), R1, FIX_CORE, {
			sentinels: ['<!DOCTYPE']
		});
	});

	it('NZBP-R1-06 subset opener outside quotes fails', () => {
		checkCase('NZBP-R1-06', { ok: false, errorCode: 'NZBP_DOCTYPE_NOT_ALLOWED' }, callPrescan('<!DOCTYPE nzb [<!ENTITY a "x">]><nzb/>'), R1, FIX_CORE);
	});

	it('NZBP-R1-07 unterminated quote in the doctype fails', () => {
		checkCase('NZBP-R1-07', { ok: false, errorCode: 'NZBP_DOCTYPE_NOT_ALLOWED' }, callPrescan('<!DOCTYPE nzb PUBLIC "abc><nzb/>'), R1, FIX_CORE);
	});

	it('NZBP-R1-08 entity literal anywhere fails closed, even inside a comment', () => {
		// Accepted trade-off: the literal inside comments/CDATA false-rejects.
		// Fail-closed is correct here; no region-accurate rescue allowed.
		checkCase('NZBP-R1-08', { ok: false, errorCode: 'NZBP_DOCTYPE_NOT_ALLOWED' }, callPrescan('<!-- <!ENTITY x "y"> --><nzb/>'), R1, FIX_CORE);
	});

	it('NZBP-R1-09 parameter entity fails', () => {
		checkCase('NZBP-R1-09', { ok: false, errorCode: 'NZBP_DOCTYPE_NOT_ALLOWED' }, callPrescan('<!DOCTYPE nzb [<!ENTITY % pe "x"> %pe;]><nzb/>'), R1, FIX_CORE);
	});

	it('NZBP-R1-10 excised output keeps the document and no doctype remains', () => {
		checkCase(
			'NZBP-R1-10',
			{ ok: true, documentText: '<?xml version="1.0"?><nzb><file/></nzb>' },
			callPrescan('<?xml version="1.0"?><!DOCTYPE nzb SYSTEM "s.dtd"><nzb><file/></nzb>'),
			R1,
			FIX_CORE,
			{ sentinels: ['<!DOCTYPE', 's.dtd'] }
		);
	});

	it('NZBP-R1-11 lowercase doctype token is ignored by the pre-scan', () => {
		// Exact-case only: the pre-scan passes it through and the real parser
		// rejects it later (browser bank pins the malformed outcome).
		checkCase('NZBP-R1-11', { ok: true, documentText: '<!doctype nzb><nzb/>' }, callPrescan('<!doctype nzb><nzb/>'), R1, FIX_CORE);
	});

	it('NZBP-R1-12 doctype not followed by whitespace fails', () => {
		checkCase('NZBP-R1-12', { ok: false, errorCode: 'NZBP_DOCTYPE_NOT_ALLOWED' }, callPrescan('<!DOCTYPEnzb><nzb/>'), R1, FIX_CORE);
	});

	it('NZBP-R1-14 BOM, whitespace, comment and PI prolog with bare doctype excises', () => {
		checkCase(
			'NZBP-R1-14',
			{ ok: true, documentText: '\uFEFF  <!--c--><?p q?><nzb/>' },
			callPrescan('\uFEFF  <!--c--><?p q?><!DOCTYPE nzb><nzb/>'),
			R1,
			FIX_CORE,
			{ sentinels: ['<!DOCTYPE'] }
		);
	});
});

describe('NZBP-R3 post-expansion string caps', () => {
	it('NZBP-R3-01 1025-char bytes value fails before any BigInt runs', () => {
		const root = nzbDoc([fileEntry([segment({ bytes: '9'.repeat(1025), number: '1', text: 'mid-long' })])]);
		checkCase('NZBP-R3-01', errOutcome('NZBP_TOO_MANY_NODES'), callSummarize(root), 'NZBP_CAPS', FIX_CORE, {
			sentinels: ['mid-long', '9'.repeat(64)]
		});
	});

	it('NZBP-R3-02 1025-char non-bytes attribute fails (every value checked)', () => {
		const root = nzbDoc([fileEntry([segment({ bytes: '5', number: '1', text: 'mid-ok', extra: [['poster', 'p'.repeat(1025)]] })])]);
		checkCase('NZBP-R3-02', errOutcome('NZBP_TOO_MANY_NODES'), callSummarize(root), 'NZBP_CAPS', FIX_CORE, {
			sentinels: ['mid-ok', 'p'.repeat(64)]
		});
	});

	it('NZBP-R3-05 astral characters count 2 UTF-16 code units', () => {
		const astral = '𐐀'; // U+10400: length 2 in JS
		const root = nzbDoc([fileEntry([segment({ bytes: '5', number: '1', text: 'mid-ok', extra: [['note', `${'n'.repeat(1023)}${astral}`]] })])]);
		checkCase('NZBP-R3-05', errOutcome('NZBP_TOO_MANY_NODES'), callSummarize(root), 'NZBP_CAPS', FIX_CORE, {
			sentinels: ['mid-ok']
		});
	});
});

describe('NZBP-R4 numeric semantics and check order', () => {
	it('NZBP-R4-01 empty bytes is a missing claim, never zero', () => {
		const root = nzbDoc([fileEntry([segment({ bytes: '', number: '1', text: 'mid-empty' })])]);
		checkCase(
			'NZBP-R4-01',
			okOutcome(summary({ file_entry_count: 1, segment_count: 1, declared_bytes_total: null, declared_bytes_basis: 'unknown', segments_without_byte_claim: 1 })),
			callSummarize(root),
			'NZBP_NUMERIC',
			FIX_CORE,
			{ sentinels: ['mid-empty'] }
		);
	});

	it('NZBP-R4-02 hex bytes is a missing claim', () => {
		const root = nzbDoc([fileEntry([segment({ bytes: '0x10', number: '1', text: 'mid-hex' })])]);
		checkCase(
			'NZBP-R4-02',
			okOutcome(summary({ file_entry_count: 1, segment_count: 1, declared_bytes_total: null, declared_bytes_basis: 'unknown', segments_without_byte_claim: 1 })),
			callSummarize(root),
			'NZBP_NUMERIC',
			FIX_CORE,
			{ sentinels: ['mid-hex', '0x10'] }
		);
	});

	it('NZBP-R4-03 signed bytes is a missing claim', () => {
		const root = nzbDoc([fileEntry([segment({ bytes: '+12', number: '1', text: 'mid-plus' })])]);
		checkCase(
			'NZBP-R4-03',
			okOutcome(summary({ file_entry_count: 1, segment_count: 1, declared_bytes_total: null, declared_bytes_basis: 'unknown', segments_without_byte_claim: 1 })),
			callSummarize(root),
			'NZBP_NUMERIC',
			FIX_CORE,
			{ sentinels: ['mid-plus', '+12'] }
		);
	});

	it('NZBP-R4-04 padded bytes is a missing claim', () => {
		const root = nzbDoc([fileEntry([segment({ bytes: ' 12 ', number: '1', text: 'mid-pad' })])]);
		checkCase(
			'NZBP-R4-04',
			okOutcome(summary({ file_entry_count: 1, segment_count: 1, declared_bytes_total: null, declared_bytes_basis: 'unknown', segments_without_byte_claim: 1 })),
			callSummarize(root),
			'NZBP_NUMERIC',
			FIX_CORE,
			{ sentinels: ['mid-pad'] }
		);
	});

	it('NZBP-R4-05 explicit zero bytes is a known zero total', () => {
		const root = nzbDoc([fileEntry([segment({ bytes: '0', number: '1', text: 'mid-zero' })])]);
		checkCase(
			'NZBP-R4-05',
			okOutcome(summary({ file_entry_count: 1, segment_count: 1, declared_bytes_total: '0', declared_bytes_basis: 'all_segments_claim_bytes' })),
			callSummarize(root),
			'NZBP_NUMERIC',
			FIX_CORE,
			{ sentinels: ['mid-zero'] }
		);
	});

	it('NZBP-R4-06 structural depth wins over numeric over-cap', () => {
		let deep: NzbpTestElement = segment({ bytes: '99999999999999999999', number: '1', text: 'mid-deep' });
		for (let depth = 0; depth < 16; depth += 1) deep = el('wrap', { children: [deep] });
		checkCase('NZBP-R4-06', errOutcome('NZBP_TOO_DEEP'), callSummarize(nzbDoc([deep])), 'NZBP_ORDER', FIX_CORE, {
			sentinels: ['mid-deep']
		});
	});

	it('NZBP-R4-07 over-long digit bytes fails as structure, not as over-cap', () => {
		const root = nzbDoc([fileEntry([segment({ bytes: '7'.repeat(1025), number: '1', text: 'mid-long' })])]);
		checkCase('NZBP-R4-07', errOutcome('NZBP_TOO_MANY_NODES'), callSummarize(root), 'NZBP_ORDER', FIX_CORE, {
			sentinels: ['mid-long']
		});
	});

	it('NZBP-R4-08 leading-zero numbers compare canonically', () => {
		const root = nzbDoc([fileEntry([segment({ bytes: '1', number: '001', text: 'mid-a' }), segment({ bytes: '1', number: '1', text: 'mid-b' })])]);
		checkCase(
			'NZBP-R4-08',
			okOutcome(summary({ file_entry_count: 1, segment_count: 2, declared_bytes_total: '2', declared_bytes_basis: 'all_segments_claim_bytes', duplicate_segment_numbers: 1 })),
			callSummarize(root),
			'NZBP_NUMERIC',
			FIX_CORE,
			{ sentinels: ['mid-a', 'mid-b'] }
		);
	});

	it('NZBP-R4-09 1024-digit numbers never silently collide', () => {
		const hugeA = '1'.repeat(1024);
		const hugeB = '2'.repeat(1024);
		const root = nzbDoc([
			fileEntry([
				segment({ bytes: '1', number: hugeA, text: 'mid-a' }),
				segment({ bytes: '1', number: hugeB, text: 'mid-b' }),
				segment({ bytes: '1', number: hugeA, text: 'mid-c' })
			])
		]);
		checkCase(
			'NZBP-R4-09',
			okOutcome(summary({ file_entry_count: 1, segment_count: 3, declared_bytes_total: '3', declared_bytes_basis: 'all_segments_claim_bytes', duplicate_segment_numbers: 1 })),
			callSummarize(root),
			'NZBP_NUMERIC',
			FIX_CORE,
			{ sentinels: ['mid-a', 'mid-b', 'mid-c'] }
		);
	});

	it('NZBP-R4-10 counting is case-sensitive local name anywhere, namespaces ignored below root', () => {
		const root = nzbDoc([
			el('FILE', { children: [el('segments', { children: [segment({ bytes: '1', number: '1', text: 'mid-upper' })] })] }),
			segment({ bytes: '1', number: '2', text: 'mid-lone' }),
			el('file', { ns: 'http://foreign.invalid/x', children: [el('segments', { children: [segment({ bytes: '1', number: '3', text: 'mid-foreign' })] })] })
		]);
		checkCase(
			'NZBP-R4-10',
			okOutcome(summary({ file_entry_count: 1, segment_count: 3, declared_bytes_total: '3', declared_bytes_basis: 'all_segments_claim_bytes' })),
			callSummarize(root),
			'NZBP_COUNTING',
			FIX_CORE,
			{ sentinels: ['mid-upper', 'mid-lone', 'mid-foreign'] }
		);
	});

	it('NZBP-R4-11 prefixed bytes and number attributes are ignored', () => {
		const root = nzbDoc([
			fileEntry([
				segment({ text: 'mid-pref', extra: [['p:bytes', '100'], ['p:number', '1']] }),
				segment({ bytes: '5', text: 'mid-plain' })
			])
		]);
		checkCase(
			'NZBP-R4-11',
			okOutcome(
				summary({
					file_entry_count: 1,
					segment_count: 2,
					declared_bytes_total: null,
					declared_bytes_basis: 'unknown',
					segments_without_byte_claim: 1,
					segments_without_number: 2
				})
			),
			callSummarize(root),
			'NZBP_COUNTING',
			FIX_CORE,
			{ sentinels: ['mid-pref', 'mid-plain'] }
		);
	});

	it('NZBP-R4-12 unclosed declaration in the scan window fails closed', () => {
		checkCase(
			'NZBP-R4-12',
			{ ok: false, errorCode: ['NZBP_ENCODING_UNSUPPORTED', 'NZBP_MALFORMED_XML'] },
			callPrescan('<?xml version="1.0"\n<nzb/>'),
			'NZBP_PRESCAN',
			FIX_CORE,
			{ anyOfCodes: ['NZBP_ENCODING_UNSUPPORTED', 'NZBP_MALFORMED_XML'] }
		);
	});

	it('NZBP-R4-13 XML 1.1 declaration is rejected', () => {
		checkCase('NZBP-R4-13', { ok: false, errorCode: 'NZBP_ENCODING_UNSUPPORTED' }, callPrescan('<?xml version="1.1"?><nzb/>'), 'NZBP_PRESCAN', FIX_CORE);
	});

	it('NZBP-R4-14 non-UTF-8 declared encoding is rejected', () => {
		checkCase(
			'NZBP-R4-14',
			{ ok: false, errorCode: 'NZBP_ENCODING_UNSUPPORTED' },
			callPrescan('<?xml version="1.0" encoding="ISO-8859-1"?><nzb/>'),
			'NZBP_PRESCAN',
			FIX_CORE
		);
	});

	it('NZBP-R4-15 multiple segment groups sum into one entry and warn', () => {
		const root = nzbDoc([
			el('file', {
				attrs: [['subject', SENTINELS.subject]],
				children: [
					el('segments', { children: [segment({ bytes: '1', number: '1', text: 'mid-a' })] }),
					el('segments', { children: [segment({ bytes: '2', number: '2', text: 'mid-b' })] })
				]
			})
		]);
		checkCase(
			'NZBP-R4-15',
			okOutcome(summary({ file_entry_count: 1, segment_count: 2, declared_bytes_total: '3', declared_bytes_basis: 'all_segments_claim_bytes', files_with_multiple_segment_groups: 1 })),
			callSummarize(root),
			'NZBP_SUMMARY',
			FIX_CORE,
			{ sentinels: [SENTINELS.subject, 'mid-a', 'mid-b'] }
		);
	});

	it('NZBP-R4-16 segment identity trims surrounding whitespace', () => {
		const root = nzbDoc([fileEntry([segment({ bytes: '1', number: '1', text: '  pad-id  ' }), segment({ bytes: '1', number: '2', text: 'pad-id' })])]);
		checkCase(
			'NZBP-R4-16',
			okOutcome(summary({ file_entry_count: 1, segment_count: 2, declared_bytes_total: '2', declared_bytes_basis: 'all_segments_claim_bytes', duplicate_message_ids: 1 })),
			callSummarize(root),
			'NZBP_SUMMARY',
			FIX_CORE,
			{ sentinels: ['pad-id'] }
		);
	});

	it('NZBP-R4-18 file-count cap wins over numeric over-cap', () => {
		const files = Array.from({ length: 5001 }, () => fileEntry([segment({ bytes: '99999999999999999999', number: '1', text: 'mid-x' })], [['subject', 'bulk']]));
		checkCase('NZBP-R4-18', errOutcome('NZBP_TOO_MANY_FILES'), callSummarize(nzbDoc(files)), 'NZBP_ORDER', FIX_CORE, {
			sentinels: ['mid-x']
		});
	});

	it('NZBP-R4-19 declaration positives pass (absent, 1.0, case, quotes, spacing, BOM)', () => {
		const variants: [string, string][] = [
			['plain', '<nzb/>'],
			['decl', '<?xml version="1.0"?><nzb/>'],
			['lower_encoding', '<?xml version="1.0" encoding="utf-8"?><nzb/>'],
			['single_quotes', "<?xml version='1.0'?><nzb/>"],
			['spaced', '<?xml   version = "1.0"   ?><nzb/>'],
			['bom', '\uFEFF<?xml version="1.0"?><nzb/>']
		];
		const actual: Record<string, unknown> = {};
		for (const [name, input] of variants) actual[name] = callPrescan(input);
		checkCase(
			'NZBP-R4-19',
			{
				plain: { ok: true, documentText: '<nzb/>' },
				decl: { ok: true, documentText: '<?xml version="1.0"?><nzb/>' },
				lower_encoding: { ok: true, documentText: '<?xml version="1.0" encoding="utf-8"?><nzb/>' },
				single_quotes: { ok: true, documentText: "<?xml version='1.0'?><nzb/>" },
				spaced: { ok: true, documentText: '<?xml   version = "1.0"   ?><nzb/>' },
				bom: { ok: true, documentText: '\uFEFF<?xml version="1.0"?><nzb/>' }
			},
			actual,
			'NZBP_PRESCAN',
			FIX_CORE
		);
	});

	it('NZBP-R4-20 file without segments counts and warns', () => {
		const root = nzbDoc([el('file'), el('file', { children: [el('segments', { children: [] })] })]);
		checkCase(
			'NZBP-R4-20',
			okOutcome(summary({ file_entry_count: 2, files_without_segments: 2 })),
			callSummarize(root),
			'NZBP_SUMMARY',
			FIX_CORE
		);
	});

	it('NZBP-R4-21 leading-zero bytes contribute their value', () => {
		const root = nzbDoc([fileEntry([segment({ bytes: '007', number: '1', text: 'mid-zero' })])]);
		checkCase(
			'NZBP-R4-21',
			okOutcome(summary({ file_entry_count: 1, segment_count: 1, declared_bytes_total: '7', declared_bytes_basis: 'all_segments_claim_bytes' })),
			callSummarize(root),
			'NZBP_NUMERIC',
			FIX_CORE,
			{ sentinels: ['mid-zero'] }
		);
	});

	it('NZBP-R4-22 same number across file entries is not a duplicate', () => {
		const root = nzbDoc([fileEntry([segment({ bytes: '1', number: '9', text: 'mid-a' })]), fileEntry([segment({ bytes: '1', number: '9', text: 'mid-b' })])]);
		checkCase(
			'NZBP-R4-22',
			okOutcome(summary({ file_entry_count: 2, segment_count: 2, declared_bytes_total: '2', declared_bytes_basis: 'all_segments_claim_bytes', duplicate_segment_numbers: 0 })),
			callSummarize(root),
			'NZBP_SUMMARY',
			FIX_CORE,
			{ sentinels: ['mid-a', 'mid-b'] }
		);
	});

	it('NZBP-R4-23 file-level bytes attribute is ignored', () => {
		const root = nzbDoc([el('file', { attrs: [['bytes', '100']], children: [el('segments', { children: [segment({ bytes: '4', number: '1', text: 'mid-a' })] })] })]);
		checkCase(
			'NZBP-R4-23',
			okOutcome(summary({ file_entry_count: 1, segment_count: 1, declared_bytes_total: '4', declared_bytes_basis: 'all_segments_claim_bytes' })),
			callSummarize(root),
			'NZBP_COUNTING',
			FIX_CORE,
			{ sentinels: ['mid-a'] }
		);
	});

	it('NZBP-R4-24 root namespace positives pass (absent and newzbin)', () => {
		const actual = {
			absent: callSummarize(el('nzb', { children: [] })),
			newzbin: callSummarize(el('nzb', { ns: NZB_NAMESPACE, children: [] }))
		};
		checkCase('NZBP-R4-24', { absent: okOutcome(summary()), newzbin: okOutcome(summary()) }, actual, 'NZBP_NOT_NZB', FIX_CORE);
	});
});

describe('NZBP-R5 outcome shape and retention', () => {
	it('NZBP-R5-01 summary key allowlist and error shape', () => {
		const okActual = callSummarize(nzbDoc([fileEntry([segment({ bytes: '1', number: '1', text: 'mid-a' })])])) as {
			status?: unknown;
			error_code?: unknown;
			next_action?: unknown;
			summary?: Record<string, unknown>;
		} | null;
		const errActual = callSummarize(el('rss')) as Record<string, unknown> | null;
		checkCase(
			'NZBP-R5-01',
			{
				okKeys: ['error_code', 'next_action', 'status', 'summary'],
				summaryKeys: [
					'declared_bytes_basis',
					'declared_bytes_total',
					'duplicate_message_ids',
					'duplicate_segment_numbers',
					'file_entry_count',
					'files_with_multiple_segment_groups',
					'files_without_segments',
					'preview_config_version',
					'segment_count',
					'segments_without_byte_claim',
					'segments_without_number'
				],
				errKeys: ['error_code', 'next_action', 'status']
			},
			{
				okKeys: okActual && typeof okActual === 'object' ? Object.keys(okActual).sort() : [],
				summaryKeys: okActual?.summary && typeof okActual.summary === 'object' ? Object.keys(okActual.summary).sort() : [],
				errKeys: errActual && typeof errActual === 'object' ? Object.keys(errActual).sort() : []
			},
			'NZBP_SHAPE',
			FIX_CORE
		);
	});

	it('NZBP-R5-02 attribute values are length-checked but never returned', () => {
		const root = nzbDoc([
			el('file', {
				attrs: [
					['subject', SENTINELS.subject],
					['poster', SENTINELS.poster]
				],
				children: [
					el('groups', { children: [el('group', { text: SENTINELS.group })] }),
					el('segments', { children: [segment({ bytes: '1', number: '1', text: `${SENTINELS.mid}-kept` })] })
				]
			})
		]);
		checkCase(
			'NZBP-R5-02',
			okOutcome(summary({ file_entry_count: 1, segment_count: 1, declared_bytes_total: '1', declared_bytes_basis: 'all_segments_claim_bytes' })),
			callSummarize(root),
			'NZBP_SHAPE',
			FIX_CORE,
			{ sentinels: [SENTINELS.subject, SENTINELS.poster, SENTINELS.group, `${SENTINELS.mid}-kept`] }
		);
	});
});

describe('NZBP drift and controls', () => {
	function rustConstNumber(source: string, name: string, type: string): number {
		const match = source.match(new RegExp(`pub const ${name}:\\s*${type}\\s*=\\s*([^;]+);`));
		if (!match?.[1]) throw new Error(`intake constant missing: ${name}`);
		const expression = match[1].trim();
		if (!/^[\d\s_*]+$/.test(expression)) throw new Error(`intake constant not a plain product: ${name}`);
		return expression
			.split('*')
			.map((part) => Number(part.replaceAll('_', '').trim()))
			.reduce((product, factor) => product * factor, 1);
	}

	function rustConstInt(source: string, name: string, type: string): number {
		const match = source.match(new RegExp(`pub const ${name}:\\s*${type}\\s*=\\s*([^;]+);`));
		if (!match?.[1]) throw new Error(`intake constant missing: ${name}`);
		return Number(match[1].replaceAll('_', '').trim());
	}

	const intake = {
		version: rustConstInt(nzbIntakeRaw, 'INTAKE_CONFIG_VERSION', 'u32'),
		maxBytes: rustConstNumber(nzbIntakeRaw, 'MAX_NZB_BYTES', 'usize'),
		maxFiles: rustConstInt(nzbIntakeRaw, 'MAX_FILES', 'usize'),
		maxSegments: rustConstInt(nzbIntakeRaw, 'MAX_SEGMENTS', 'usize'),
		maxDeclaredBytes: rustConstNumber(nzbIntakeRaw, 'MAX_DECLARED_BYTES', 'u64')
	};

	it('NZBP-CTRL-01 intake constants parse to the known v1 policy', () => {
		checkCase(
			'NZBP-CTRL-01',
			{ version: 1, maxBytes: 2097152, maxFiles: 5000, maxSegments: 60000, maxDeclaredBytes: 68719476736 },
			intake,
			'NZBP_HARNESS',
			'The intake source moved; send this case back to the acceptance owner.'
		);
	});

	it('NZBP-CTRL-02 web import cap equals the intake byte cap', () => {
		checkCase('NZBP-CTRL-02', { webCap: intake.maxBytes }, { webCap: MAX_NZB_IMPORT_BYTES }, 'NZBP_HARNESS', 'The web cap drifted; send this case back to the acceptance owner.');
	});

	it('NZBP-DRIFT-01 preview limits mirror the intake v1 caps', () => {
		const limits = (previewModule?.NZBP_LIMITS ?? {}) as Record<string, unknown>;
		const actual = {
			maxBytes: limits.maxBytes ?? 'missing',
			maxFiles: limits.maxFiles ?? 'missing',
			maxSegments: limits.maxSegments ?? 'missing',
			maxDeclaredBytes: limits.maxDeclaredBytes === undefined ? 'missing' : Number(limits.maxDeclaredBytes as number | bigint)
		};
		checkCase(
			'NZBP-DRIFT-01',
			{ maxBytes: intake.maxBytes, maxFiles: intake.maxFiles, maxSegments: intake.maxSegments, maxDeclaredBytes: intake.maxDeclaredBytes },
			actual,
			'NZBP_DRIFT',
			FIX_CORE
		);
	});

	it('NZBP-DRIFT-02 preview version and namespace pin to intake v1', () => {
		checkCase(
			'NZBP-DRIFT-02',
			{ version: intake.version, namespace: NZB_NAMESPACE },
			{ version: previewModule?.NZBP_PREVIEW_CONFIG_VERSION ?? 'missing', namespace: previewModule?.NZBP_NZB_NAMESPACE ?? 'missing' },
			'NZBP_DRIFT',
			FIX_CORE
		);
	});
});

describe('NZBP page and component mechanisms', () => {
	it('NZBP-SRC-01 read path accounts actual bytes, not file size alone', () => {
		checkCase('NZBP-SRC-01', { actualBytes: true }, { actualBytes: previewUiRaw.includes('byteLength') }, 'NZBP_MECHANISM', FIX_CORE);
	});

	it('NZBP-SRC-02 abort path releases the stream reader', () => {
		checkCase(
			'NZBP-SRC-02',
			{ cancelsReader: true },
			{ cancelsReader: /reader\??\.cancel\(/.test(previewUiRaw) },
			'NZBP_MECHANISM',
			FIX_CORE
		);
	});

	it('NZBP-SRC-03 decode is fatal UTF-8', () => {
		checkCase(
			'NZBP-SRC-03',
			{ fatalUtf8: true },
			{ fatalUtf8: previewUiRaw.includes('TextDecoder') && previewUiRaw.includes('fatal') },
			'NZBP_MECHANISM',
			FIX_CORE
		);
	});

	it('NZBP-SRC-04 preview sources never use innerHTML, html injection or XMLSerializer', () => {
		const combined = `${previewUiRaw}\n${previewCoreRaw}`;
		checkCase(
			'NZBP-SRC-04',
			{ previewModuleExists: true, clean: true },
			{
				previewModuleExists: previewModuleRaw !== undefined,
				clean: !combined.includes('innerHTML') && !combined.includes('{@html}') && !combined.includes('XMLSerializer')
			},
			'NZBP_MECHANISM',
			FIX_CORE
		);
	});

	it('NZBP-SRC-05 built-in parser adapter, pure core, no new XML dependency', () => {
		const packageJson = JSON.parse(packageRaw) as { dependencies?: Record<string, string>; devDependencies?: Record<string, string> };
		const dependencyNames = [...Object.keys(packageJson.dependencies ?? {}), ...Object.keys(packageJson.devDependencies ?? {})];
		checkCase(
			'NZBP-SRC-05',
			{ adapterUsesDomParser: true, coreDomFree: true, xmlDependency: null },
			{
				adapterUsesDomParser: previewUiRaw.includes('DOMParser'),
				coreDomFree: previewModuleRaw !== undefined && !previewModuleRaw.includes('DOMParser'),
				xmlDependency: dependencyNames.find((name) => name.toLowerCase().includes('xml')) ?? null
			},
			'NZBP_MECHANISM',
			FIX_CORE
		);
	});

	it('NZBP-SRC-06 no preview API route exists', () => {
		const previewRoutes = apiRouteKeys.filter((key) => /preview|nzbp/i.test(key));
		checkCase('NZBP-SRC-06', { previewRoutes: [] }, { previewRoutes }, 'NZBP_MECHANISM', FIX_CORE);
	});
});
