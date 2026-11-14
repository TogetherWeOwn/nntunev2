/** One owner's retained download URL; callers own formatting, feedback publication and teardown. */
export function createM3UDownload() {
	let url: string | null = null;

	function release(): void {
		if (url !== null) URL.revokeObjectURL(url);
		url = null;
	}

	function download(content: () => string, filename: string, savedDownloads = false): string {
		const link = document.createElement('a');
		try {
			// Do not revoke on a timer while the browser may still be saving.
			release();
			url = URL.createObjectURL(new Blob([content()], { type: 'audio/x-mpegurl;charset=utf-8' }));
			link.href = url;
			link.download = filename;
			link.hidden = true;
			document.body.append(link);
			link.click();
		} finally {
			link.remove();
		}
		return `Download started. Check your browser’s ${savedDownloads ? 'saved downloads' : 'downloads'} for ${filename}.`;
	}

	return { download, release };
}

export function m3uDownloadFailure(subject: string): string {
	return `Could not start the download. Try again or copy the ${subject} as text.`;
}
