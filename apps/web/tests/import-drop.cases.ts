import { MAX_NZB_IMPORT_BYTES } from '../src/lib/library/import-validation.js';

// Synthetic metadata only. Shared by the validator units and real-page browser
// cases; the production validator remains the sole source of the size ceiling.
export const dropCases = [
	{ id: 'DROP-01', name: 'local-album.nzb', size: 1234, error: null },
	{ id: 'DROP-02', name: 'UPPERCASE.NZB', size: 17, error: null },
	{ id: 'DROP-03', name: 'at-cap.nzb', size: MAX_NZB_IMPORT_BYTES, error: null },
	{ id: 'DROP-04', name: 'empty.nzb', size: 0, error: /empty|no data|0 bytes/i },
	{
		id: 'DROP-05',
		name: 'over-cap.nzb',
		size: MAX_NZB_IMPORT_BYTES + 1,
		error: /too (large|big)|size|limit|exceed|maximum/i
	},
	{ id: 'DROP-06', name: 'album.nzb.txt', size: 31, error: /nzb|extension|type/i }
] as const;
