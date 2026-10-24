import { validateImportFile } from './import-validation.js';

export type ImportSelection = { status: string | null; error: string | null };

function formatSize(byteCount: number): string {
	if (byteCount >= 1024 * 1024) return `${(byteCount / 1024 / 1024).toFixed(2)} MiB`;
	if (byteCount >= 1024) return `${(byteCount / 1024).toFixed(1)} KiB`;
	return `${byteCount} bytes`;
}

/** Shared picker/drop metadata path. No File contents or MIME type are inspected. */
export function selectImportFiles(files: readonly File[]): ImportSelection {
	if (files.length !== 1) {
		return {
			status: null,
			error: files.length > 1
				? 'Choose or drop one NZB file at a time, not multiple files.'
				: 'Choose or drop one local NZB file. Folders, links and text are not supported.'
		};
	}
	const file = files[0];
	const error = validateImportFile(file);
	if (error) return { status: null, error };
	return {
		error: null,
		status: `${file.name} (${formatSize(file.size)}) selected locally — import is not available yet. This page does not upload or process the file or add tracks to your library.`
	};
}
