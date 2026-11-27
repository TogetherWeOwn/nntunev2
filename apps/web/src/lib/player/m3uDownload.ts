/** One owner's retained URL; callers own the link lifecycle, feedback and teardown. */
export function createM3UDownload() {
	let url: string | null = null;

	function release(): void {
		const previous = url;
		url = null;
		if (previous === null) return;
		try { URL.revokeObjectURL(previous); } catch { /* Teardown is best effort. */ }
	}

	function download(link: HTMLAnchorElement, content: () => string, filename: string, savedDownloads = false): string {
		// Do not revoke on a timer while the browser may still be saving.
		release();
		const created = url = URL.createObjectURL(new Blob([content()], { type: 'audio/x-mpegurl;charset=utf-8' }));
		try {
			link.href = created;
			link.download = filename;
			link.hidden = true;
			document.body.append(link);
			link.click();
		} catch (error) {
			if (url === created) release();
			throw error;
		}
		return `Download started. Check your browser’s ${savedDownloads ? 'saved downloads' : 'downloads'} for ${filename}.`;
	}

	return { download, release };
}

export function m3uDownloadFailure(subject: string): string {
	return `Could not start the download. Try again or copy the ${subject} as text.`;
}
