import { MAX_NZB_IMPORT_BYTES } from './import-validation.js';

export const NZBP_PREVIEW_CONFIG_VERSION = 1;
export const NZBP_NZB_NAMESPACE = 'http://www.newzbin.com/DTD/2003/nzb';
export const NZBP_LIMITS = Object.freeze({
	maxBytes: MAX_NZB_IMPORT_BYTES,
	maxFiles: 5000,
	maxSegments: 60000,
	maxDeclaredBytes: 64 * 1024 * 1024 * 1024,
	maxDepth: 16,
	maxElements: 150000,
	maxAttributesPerElement: 16,
	maxAttributeValueChars: 1024,
	maxSegmentTextChars: 512,
	xmlDeclarationScanBytes: 256
});

import type { NZBP_ERROR_TEXT } from './nzb-preview-copy.js';
export type NzbpErrorCode = keyof typeof NZBP_ERROR_TEXT;
export type NzbpSummary = {
	preview_config_version: number;
	file_entry_count: number;
	segment_count: number;
	declared_bytes_total: string | null;
	declared_bytes_basis: 'all_segments_claim_bytes' | 'unknown' | 'no_segments';
	segments_without_byte_claim: number;
	segments_without_number: number;
	duplicate_segment_numbers: number;
	duplicate_message_ids: number;
	files_without_segments: number;
	files_with_multiple_segment_groups: number;
};
export type NzbpOutcome =
	| { status: 'ok'; error_code: null; next_action: 'none'; summary: NzbpSummary }
	| { status: 'error'; error_code: NzbpErrorCode; next_action: 'none' | 'try_again' | 'choose_different_file' };

export function nzbpError(code: NzbpErrorCode): NzbpOutcome {
	return {
		status: 'error', error_code: code,
		next_action: code === 'NZBP_READ_FAILED' ? 'try_again' : code === 'NZBP_PARSER_UNAVAILABLE' ? 'none' : 'choose_different_file'
	};
}

type Prescan = { ok: true; documentText: string } | { ok: false; errorCode: NzbpErrorCode };
const space = (char: string | undefined) => char !== undefined && ' \t\r\n'.includes(char);
const scanError = (errorCode: NzbpErrorCode): Prescan => ({ ok: false, errorCode });

/** Fail-closed prolog framing, not an XML tokenizer. Entity literals in comments
 * and CDATA deliberately false-reject: no attempt is made to rescue them. */
export function prescanNzbProlog(text: string): Prescan {
	if (text.includes('<!ENTITY')) return scanError('NZBP_DOCTYPE_NOT_ALLOWED');
	// Conservative parser-exposure bound, over the ORIGINAL pre-excision text.
	// Every literal opener counts (closing tags, comments, CDATA and PIs too).
	// Constant auxiliary memory; deliberately no region-aware rescue or token array.
	let markupOpeners = 0;
	for (let i = 0; i < text.length; i++) {
		if (text.charCodeAt(i) === 60 && ++markupOpeners > NZBP_LIMITS.maxElements) {
			return scanError('NZBP_TOO_MANY_NODES');
		}
	}
	let cursor = text.startsWith('﻿') ? 1 : 0;
	if (text.startsWith('<?xml', cursor) && space(text[cursor + 5])) {
		const end = text.indexOf('?>', cursor + 5);
		if (end < 0 || new TextEncoder().encode(text.slice(0, end + 2)).byteLength > NZBP_LIMITS.xmlDeclarationScanBytes) {
			return scanError('NZBP_ENCODING_UNSUPPORTED');
		}
		const declaration = text.slice(cursor, end + 2);
		// Bounded to 256 bytes; fixed, non-nested patterns tolerate both quotes and XML whitespace.
		const match = /^<\?xml[\t\r\n ]+version[\t\r\n ]*=[\t\r\n ]*(['"])([^'"]*)\1(?:[\t\r\n ]+encoding[\t\r\n ]*=[\t\r\n ]*(['"])([^'"]*)\3)?(?:[\t\r\n ]+standalone[\t\r\n ]*=[\t\r\n ]*(['"])(yes|no)\5)?[\t\r\n ]*\?>$/.exec(declaration);
		if (!match || match[2] !== '1.0' || (match[4] !== undefined && match[4].toUpperCase() !== 'UTF-8')) {
			return scanError('NZBP_ENCODING_UNSUPPORTED');
		}
		cursor = end + 2;
	}
	let start = -1;
	let end = -1;
	while (cursor < text.length) {
		if (space(text[cursor])) { cursor++; continue; }
		if (text.startsWith('<!--', cursor)) {
			const close = text.indexOf('-->', cursor + 4);
			if (close < 0) return scanError('NZBP_MALFORMED_XML');
			cursor = close + 3;
			continue;
		}
		if (text.startsWith('<?', cursor)) {
			const close = text.indexOf('?>', cursor + 2);
			if (close < 0) return scanError('NZBP_MALFORMED_XML');
			cursor = close + 2;
			continue;
		}
		if (!text.startsWith('<!DOCTYPE', cursor)) break;
		if (start >= 0 || !space(text[cursor + 9])) return scanError('NZBP_DOCTYPE_NOT_ALLOWED');
		start = cursor;
		cursor += 9;
		const bodyStart = cursor;
		let quote = '';
		for (; cursor < text.length; cursor++) {
			const char = text[cursor];
			if (quote) { if (char === quote) quote = ''; }
			else if (char === '"' || char === "'") quote = char;
			else if (char === '[' || char === '<') return scanError('NZBP_DOCTYPE_NOT_ALLOWED');
			else if (char === '>') break;
		}
		if (cursor === text.length || quote) return scanError('NZBP_DOCTYPE_NOT_ALLOWED');
		// Only subset-free bare/SYSTEM/PUBLIC declarations. Names are framed, never retained.
		const body = text.slice(bodyStart, cursor);
		if (!/^[\t\r\n ]+[A-Za-z_:][A-Za-z0-9_.:-]*(?:[\t\r\n ]+(?:SYSTEM[\t\r\n ]+(?:"[^"]*"|'[^']*')|PUBLIC[\t\r\n ]+(?:"[^"]*"|'[^']*')[\t\r\n ]+(?:"[^"]*"|'[^']*')))?[\t\r\n ]*$/.test(body)) {
			return scanError('NZBP_DOCTYPE_NOT_ALLOWED');
		}
		end = ++cursor;
	}
	const documentText = start < 0 ? text : text.slice(0, start) + text.slice(end);
	// The exact parser input must contain no DTD token, including hidden/trailing literals.
	if (documentText.includes('<!DOCTYPE')) return scanError('NZBP_DOCTYPE_NOT_ALLOWED');
	return { ok: true, documentText };
}

/** Minimal lazy adapter. Non-numeric attribute values are inspected for length
 * only; caller-owned nodes and duplicate identities are discarded after summary. */
export type NzbpElement = {
	readonly localName: string;
	readonly namespaceURI: string | null;
	readonly attributes: ArrayLike<{ readonly name: string; readonly value: string }>;
	readonly childElements: Iterable<NzbpElement>;
	readonly textContent: string;
};

type Entry = { segments: number; groups: number; numbers: Set<string> };
export function summarizeNzbElements(root: NzbpElement): NzbpOutcome {
	if (root.localName !== 'nzb' || (root.namespaceURI && root.namespaceURI !== NZBP_NZB_NAMESPACE)) return nzbpError('NZBP_NOT_NZB');
	const summary: NzbpSummary = {
		preview_config_version: NZBP_PREVIEW_CONFIG_VERSION,
		file_entry_count: 0, segment_count: 0,
		declared_bytes_total: null, declared_bytes_basis: 'no_segments',
		segments_without_byte_claim: 0, segments_without_number: 0,
		duplicate_segment_numbers: 0, duplicate_message_ids: 0,
		files_without_segments: 0, files_with_multiple_segment_groups: 0
	};
	// Two passes make the structural-before-numeric order global, not sibling-order dependent.
	// Iterator frames keep the explicit stack bounded by depth, even for very wide documents.
	const nodes: { node: NzbpElement; entry: Entry | null }[] = [];
	const entries: Entry[] = [];
	const frames: { children: Iterator<NzbpElement>; depth: number; entry: Entry | null }[] = [
		{ children: [root][Symbol.iterator](), depth: 1, entry: null }
	];
	let elementCount = 0;
	while (frames.length) {
		const frame = frames[frames.length - 1];
		const next = frame.children.next();
		if (next.done) { frames.pop(); continue; }
		const node = next.value;
		if (frame.depth > NZBP_LIMITS.maxDepth) return nzbpError('NZBP_TOO_DEEP');
		if (++elementCount > NZBP_LIMITS.maxElements) return nzbpError('NZBP_TOO_MANY_NODES');
		let entry = frame.entry;
		if (node.localName === 'file') {
			if (++summary.file_entry_count > NZBP_LIMITS.maxFiles) return nzbpError('NZBP_TOO_MANY_FILES');
			entry = { segments: 0, groups: 0, numbers: new Set() };
			entries.push(entry);
		} else if (node.localName === 'segment') {
			if (++summary.segment_count > NZBP_LIMITS.maxSegments) return nzbpError('NZBP_TOO_MANY_SEGMENTS');
			if (entry) entry.segments++;
		} else if (node.localName === 'segments' && entry) entry.groups++;
		nodes.push({ node, entry });
		frames.push({ children: node.childElements[Symbol.iterator](), depth: frame.depth + 1, entry });
	}
	// All string limits measure post-expansion UTF-16 code units. Internal entities
	// cannot survive prescan; numeric/predefined references cannot amplify source size.
	for (const { node } of nodes) {
		if (node.attributes.length > NZBP_LIMITS.maxAttributesPerElement) return nzbpError('NZBP_TOO_MANY_NODES');
		for (let i = 0; i < node.attributes.length; i++) {
			if (node.attributes[i].value.length > NZBP_LIMITS.maxAttributeValueChars) return nzbpError('NZBP_TOO_MANY_NODES');
		}
		if (node.localName === 'segment' && node.textContent.trim().length > NZBP_LIMITS.maxSegmentTextChars) return nzbpError('NZBP_TOO_MANY_NODES');
	}
	let total = 0n;
	const cap = BigInt(NZBP_LIMITS.maxDeclaredBytes);
	const ids = new Set<string>();
	for (const { node, entry } of nodes) {
		if (node.localName !== 'segment') continue;
		let bytes: string | undefined;
		let number: string | undefined;
		for (let i = 0; i < node.attributes.length; i++) {
			const attribute = node.attributes[i];
			if (attribute.name === 'bytes') bytes = attribute.value;
			if (attribute.name === 'number') number = attribute.value;
		}
		if (bytes !== undefined && /^[0-9]+$/.test(bytes)) {
			const claim = BigInt(bytes);
			if (claim > cap) return nzbpError('NZBP_DECLARED_BYTES_OVER_CAP');
			total += claim;
			if (total > cap) return nzbpError('NZBP_DECLARED_BYTES_OVER_CAP');
		} else summary.segments_without_byte_claim++;
		if (number !== undefined && /^[0-9]+$/.test(number)) {
			const canonical = number.replace(/^0+/, '') || '0';
			if (entry?.numbers.has(canonical)) summary.duplicate_segment_numbers++;
			entry?.numbers.add(canonical);
		} else summary.segments_without_number++;
		const id = node.textContent.trim();
		if (ids.has(id)) summary.duplicate_message_ids++;
		ids.add(id);
	}
	for (const entry of entries) {
		if (entry.segments === 0) summary.files_without_segments++;
		if (entry.groups > 1) summary.files_with_multiple_segment_groups++;
	}
	if (summary.segment_count > 0) {
		summary.declared_bytes_basis = summary.segments_without_byte_claim ? 'unknown' : 'all_segments_claim_bytes';
		if (!summary.segments_without_byte_claim) summary.declared_bytes_total = total.toString();
	}
	return { status: 'ok', error_code: null, next_action: 'none', summary };
}
