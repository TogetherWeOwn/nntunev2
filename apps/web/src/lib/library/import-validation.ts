// Mirrors MAX_NZB_BYTES in workers/ingest/src/nzb_intake.rs.
export const MAX_NZB_IMPORT_BYTES = 2 * 1024 * 1024;

/** File metadata checks only; the intake Worker still owns content validation. */
export function validateImportFile(file: File): string | null {
	if (file.size === 0) return 'This file is empty. Choose an NZB file with data.';
	if (file.size > MAX_NZB_IMPORT_BYTES) {
		return 'This file is too large. Choose an NZB file of 2 MiB or less.';
	}
	if (!file.name.toLowerCase().endsWith('.nzb')) return 'Choose a file with the .nzb extension.';
	return null;
}
