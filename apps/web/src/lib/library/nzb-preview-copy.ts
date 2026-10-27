export const NZBP_ERROR_TEXT = {
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
} as const;
