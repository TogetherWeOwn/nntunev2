import { describe, expect, it } from 'vitest';
import { MAX_NZB_IMPORT_BYTES, validateImportFile } from './import-validation.js';
import { selectImportFiles } from './import-selection.js';

function metadataFile(name: string, size: number): File {
	const file = new File([], name);
	Object.defineProperty(file, 'size', { value: size });
	for (const method of ['text', 'arrayBuffer', 'stream', 'slice', 'bytes']) {
		Object.defineProperty(file, method, {
			value: () => { throw new Error('Selection must not read contents'); }
		});
	}
	return file;
}

describe('shared picker/drop selection', () => {
	it.each([
		['album.nzb', 1234, '1.2 KiB'],
		['UPPERCASE.NZB', 17, '17 bytes'],
		['at-cap.nzb', MAX_NZB_IMPORT_BYTES, '2.00 MiB']
	])('reports local metadata for %s', (name, size, formatted) => {
		const file = metadataFile(name, size);
		const selection = selectImportFiles([file]);
		expect(selection.error).toBeNull();
		expect(selection.status).toContain(`${name} (${formatted}) selected locally`);
		expect(selection.status).toContain('does not upload or process the file or add tracks');
		expect(selectImportFiles([file])).toEqual(selection);
	});

	it.each([
		['empty.nzb', 0], ['large.nzb', MAX_NZB_IMPORT_BYTES + 1], ['notes.txt', 31]
	])('replaces success with the authoritative validation error for %s', (name, size) => {
		let selection = selectImportFiles([metadataFile('valid.nzb', 10)]);
		expect(selection.status).not.toBeNull();
		const file = metadataFile(name, size);
		selection = selectImportFiles([file]);
		expect(selection).toEqual({ status: null, error: validateImportFile(file) });
		expect(selectImportFiles([file])).toEqual(selection);
	});

	it('rejects missing or multiple files instead of keeping prior success', () => {
		expect(selectImportFiles([])).toEqual({
			status: null,
			error: 'Choose or drop one local NZB file. Folders, links and text are not supported.'
		});
		expect(selectImportFiles([metadataFile('one.nzb', 1), metadataFile('two.nzb', 1)]))
			.toEqual({ status: null, error: 'Choose or drop one NZB file at a time, not multiple files.' });
	});
});
