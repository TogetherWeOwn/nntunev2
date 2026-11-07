import { describe, expect, it } from 'vitest';
import ts from 'typescript';

const sources = import.meta.glob('./PlayerQueue.svelte', {
	query: '?raw',
	import: 'default',
	eager: true
}) as Record<string, string>;

/**
 * Executes the component's own focus helpers: focusVisible and the four call
 * sites (focusRemoveFor, focusEmptyTarget, focusFilterEmptyTarget,
 * revealCurrent) are lifted verbatim from PlayerQueue.svelte's script, so the
 * real focusVisible is bound to each caller. Only the Svelte state cells,
 * `tick` and the DOM elements are doubles. Elements record one ordered call log:
 * focus({ preventScroll: true }) must precede scrollIntoView nearest/instant.
 * Rendered focus movement is covered by the existing queue browser specs.
 */

const FUNCTIONS = [
	'focusVisible',
	'currentRowElement',
	'focusRemoveFor',
	'focusRowControl',
	'focusEmptyTarget',
	'focusFilterEmptyTarget',
	'revealCurrent'
];

type Log = string[];

interface Target {
	name: string;
	focus: (options?: unknown) => void;
	scrollIntoView: (options?: unknown) => void;
}

function extract(): string {
	const source = sources['./PlayerQueue.svelte']!;
	const start = source.indexOf('>', source.indexOf('<script')) + 1;
	const script = source.slice(start, source.indexOf('</script', start));
	const parsed = ts.createSourceFile('PlayerQueue.ts', script, ts.ScriptTarget.Latest, true);
	const found = parsed.statements.filter(
		(statement): statement is ts.FunctionDeclaration =>
			ts.isFunctionDeclaration(statement) &&
			statement.name !== undefined &&
			FUNCTIONS.includes(statement.name.text)
	);
	expect(found.map((fn) => fn.name!.text).sort()).toEqual([...FUNCTIONS].sort());
	return ts.transpileModule(found.map((fn) => fn.getText(parsed)).join('\n'), {
		compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext }
	}).outputText;
}

function target(name: string, log: Log, extra: Record<string, unknown> = {}): Target {
	return {
		name,
		focus: (options) => log.push(`${name}.focus ${JSON.stringify(options)}`),
		scrollIntoView: (options) => log.push(`${name}.scroll ${JSON.stringify(options)}`),
		...extra
	};
}

function focusedThenScrolled(name: string): string[] {
	return [
		`${name}.focus {"preventScroll":true}`,
		`${name}.scroll {"block":"nearest","behavior":"instant"}`
	];
}

interface Cells {
	list?: unknown;
	emptyTarget?: unknown;
	filterEmptyTarget?: unknown;
	selectedIndex?: number;
	visibleEntries?: { index: number }[];
	filterQuery?: string;
	tick?: () => Promise<void>;
}

function harness(cells: Cells) {
	const body = extract();
	return new Function(
		'cells',
		`let list = cells.list ?? null;
		let emptyTarget = cells.emptyTarget ?? null;
		let filterEmptyTarget = cells.filterEmptyTarget ?? null;
		let selectedIndex = cells.selectedIndex ?? -1;
		let visibleEntries = cells.visibleEntries ?? [];
		let filterQuery = cells.filterQuery ?? '';
		const tick = cells.tick ?? (() => Promise.resolve());
		${body}
		return {
			focusRemoveFor, focusEmptyTarget, focusFilterEmptyTarget, revealCurrent,
			query: () => filterQuery
		};`
	)(cells) as {
		focusRemoveFor: (id: string) => boolean;
		focusEmptyTarget: () => void;
		focusFilterEmptyTarget: () => boolean;
		revealCurrent: () => Promise<void>;
		query: () => string;
	};
}

/** A list stub shaped like the queries the component issues. */
function listOf(rows: { id: string; remove: Target | null; current?: Target }[]) {
	return {
		querySelectorAll: () =>
			rows.map((row) => ({
				getAttribute: (name: string) => (name === 'data-track-id' ? row.id : null),
				querySelector: () => row.remove
			})),
		querySelector: () => rows.find((row) => row.current)?.current ?? null
	};
}

describe('PlayerQueue focus helpers with the real focusVisible', () => {
	it('focusRemoveFor focuses then scrolls only the matching row remove button', () => {
		const log: Log = [];
		const { focusRemoveFor } = harness({
			list: listOf([
				{ id: 'a', remove: target('remove-a', log) },
				{ id: 'b', remove: target('remove-b', log) }
			])
		});
		expect(focusRemoveFor('b')).toBe(true);
		expect(log).toEqual(focusedThenScrolled('remove-b'));
	});

	it('focusRemoveFor returns false without touching anything for a missing list, row or button', () => {
		const log: Log = [];
		expect(harness({ list: null }).focusRemoveFor('a')).toBe(false);
		const rows = [
			{ id: 'a', remove: null },
			{ id: 'b', remove: target('remove-b', log) }
		];
		const { focusRemoveFor } = harness({ list: listOf(rows) });
		expect(focusRemoveFor('missing')).toBe(false);
		expect(focusRemoveFor('a')).toBe(false);
		expect(log).toEqual([]);
	});

	it('focusEmptyTarget focuses then scrolls the empty target and tolerates null', () => {
		const log: Log = [];
		harness({ emptyTarget: target('empty', log) }).focusEmptyTarget();
		expect(log).toEqual(focusedThenScrolled('empty'));
		expect(() => harness({ emptyTarget: null }).focusEmptyTarget()).not.toThrow();
		expect(log).toHaveLength(2);
	});

	it('focusFilterEmptyTarget reports whether the filter-empty target took focus', () => {
		const log: Log = [];
		expect(harness({ filterEmptyTarget: null }).focusFilterEmptyTarget()).toBe(false);
		expect(log).toEqual([]);
		const { focusFilterEmptyTarget } = harness({ filterEmptyTarget: target('filter-empty', log) });
		expect(focusFilterEmptyTarget()).toBe(true);
		expect(log).toEqual(focusedThenScrolled('filter-empty'));
	});

	it('revealCurrent does nothing without a valid current index', async () => {
		const log: Log = [];
		const current = target('current', log);
		await harness({
			selectedIndex: -1,
			list: listOf([{ id: 'a', remove: null, current }])
		}).revealCurrent();
		expect(log).toEqual([]);
	});

	it('revealCurrent focuses then scrolls a visible current row without clearing the filter', async () => {
		const log: Log = [];
		const current = target('current', log);
		const ticks: string[] = [];
		const reveal = harness({
			selectedIndex: 1,
			filterQuery: 'keep',
			visibleEntries: [{ index: 1 }],
			list: listOf([{ id: 'b', remove: null, current }]),
			tick: async () => void ticks.push('tick')
		});
		await reveal.revealCurrent();
		expect(reveal.query()).toBe('keep');
		expect(ticks).toEqual([]);
		expect(log).toEqual(focusedThenScrolled('current'));
	});

	it('revealCurrent clears a hiding filter and waits a tick before resolving the row', async () => {
		const log: Log = [];
		const current = target('current', log);
		let rendered = false;
		const list = {
			querySelector: () => {
				log.push(`lookup rendered=${rendered}`);
				return rendered ? current : null;
			}
		};
		const reveal = harness({
			selectedIndex: 2,
			filterQuery: 'zzz',
			visibleEntries: [{ index: 0 }],
			list,
			tick: async () => {
				log.push('tick');
				rendered = true;
			}
		});
		await reveal.revealCurrent();
		expect(reveal.query()).toBe('');
		expect(log).toEqual(['tick', 'lookup rendered=true', ...focusedThenScrolled('current')]);
	});

	it('revealCurrent stops quietly when the row is still absent after the tick', async () => {
		const log: Log = [];
		const reveal = harness({
			selectedIndex: 0,
			filterQuery: 'zzz',
			visibleEntries: [],
			list: { querySelector: () => null }
		});
		await expect(reveal.revealCurrent()).resolves.toBeUndefined();
		expect(reveal.query()).toBe('');
		expect(log).toEqual([]);
	});
});
