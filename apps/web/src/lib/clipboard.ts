export interface ClipboardWriter {
	writeText: (text: string) => Promise<void>;
}

/** Inspect only on activation; even the clipboard or method getter may refuse. */
export function clipboardWriter(): ClipboardWriter | undefined {
	try {
		const candidate = navigator.clipboard;
		return typeof candidate?.writeText === 'function' ? candidate : undefined;
	} catch {
		return undefined;
	}
}
