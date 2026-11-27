import ts from 'typescript';
import { expect, test, vi } from 'vitest';
import { createM3UDownload, m3uDownloadFailure } from '../player/m3uDownload.js';
import { createQueue, formatQueueM3U, formatQueueText } from '../player/queue.js';
import { appendSelection } from './selection.js';

test('a failed anchor creation reports failure and retires the preceding download', () => {
	const sources = import.meta.glob('./SelectionExport.svelte', { query: '?raw', import: 'default', eager: true });
	const source = String(sources['./SelectionExport.svelte']);
	const script = source.slice(source.indexOf('>') + 1, source.indexOf('</script>'));
	const parsed = ts.createSourceFile('selection.ts', script, ts.ScriptTarget.Latest, true);
	const body = parsed.statements.filter((node) => !ts.isImportDeclaration(node)).map((node) => node.getText(parsed));
	const { outputText } = ts.transpileModule(body.join('\n'), {
		compilerOptions: { target: ts.ScriptTarget.ES2022 }
	});
	const link = { click: vi.fn(), remove: vi.fn() };
	const createElement = vi.fn().mockReturnValueOnce(link).mockImplementationOnce(() => { throw new Error('Blocked'); });
	const revoke = vi.spyOn(URL, 'revokeObjectURL');
	const bindings = {
		createM3UDownload, m3uDownloadFailure, createQueue, formatQueueM3U, formatQueueText, appendSelection,
		document: { createElement },
		$props: () => ({ items: [{ id: 'one', title: 'Song' }], selectedIds: ['one'] }),
		$state: <T>(value: T) => value,
		$derived: <T>(value: T) => value,
		$effect: () => {},
		onDestroy: () => {}
	};
	vi.stubGlobal('document', { body: { append: vi.fn() } });
	try {
		const { downloadSelectedM3U, message } = new Function(...Object.keys(bindings),
			`${outputText}\nreturn { downloadSelectedM3U, message: () => status };`)(...Object.values(bindings));
		downloadSelectedM3U();
		expect(link.click).toHaveBeenCalledOnce();
		expect(message()).toMatch(/Download started/);
		expect(downloadSelectedM3U).not.toThrow();
		expect(message()).toMatch(/Could not start the download/);
		expect(revoke).toHaveBeenCalledWith(expect.stringMatching(/^blob:/));
	} finally {
		revoke.mockRestore();
		vi.unstubAllGlobals();
	}
});
