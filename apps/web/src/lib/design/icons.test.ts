import { compile } from 'svelte/compiler';
import { render } from 'svelte/server';
import { minify } from 'terser';
import ts from 'typescript';
import { describe, expect, it } from 'vitest';
import Icon from './Icon.svelte';
import { ICON_NAMES, ICON_PATHS } from './icons.js';

const sources = import.meta.glob('./{icons.ts,strings.ts,Icon.svelte}', {
	query: '?raw',
	import: 'default',
	eager: true
}) as Record<string, string>;

/**
 * Icon-subset acceptance: the manifest is complete and clean, the
 * component renders an honest svg in both modes, and the added script
 * stays inside the 8 KB budget.
 *
 * Budget method: compile the three modules for the client and minify
 * each independently, retaining every export. Count uncompressed UTF-8
 * bytes, including the component's runtime imports; the shared Svelte
 * runtime is already part of the app. No gzip or route tree-shaking
 * discount, and no source-text comment stripping that could eat strings.
 */

const BUDGET_BYTES = 8192;

async function scriptBytes(source: string, filename: string): Promise<number> {
	const code = filename.endsWith('.svelte')
		? compile(source, { filename, generate: 'client', dev: false }).js.code
		: ts.transpileModule(source, {
				compilerOptions: { target: ts.ScriptTarget.ESNext, module: ts.ModuleKind.ESNext }
			}).outputText;
	const result = await minify(code, { module: true, format: { comments: false } });
	if (!result.code) throw new Error(`No compiled script for ${filename}`);
	return new TextEncoder().encode(result.code).byteLength;
}

describe('icon subset: manifest', () => {
	it('ships the R1/R2 set: 50 names, each with stencil data', () => {
		expect(ICON_NAMES).toHaveLength(50);
		for (const name of ICON_NAMES) {
			expect(ICON_PATHS[name], `${name} has stencil data`).toMatch(/\S/);
		}
	});

	it('names are unique kebab-case', () => {
		expect(new Set(ICON_NAMES).size).toBe(ICON_NAMES.length);
		for (const name of ICON_NAMES) {
			expect(name, `${name} is kebab-case`).toMatch(/^[a-z0-9]+(-[a-z0-9]+)*$/);
		}
	});

	it('stencils use only static shapes, no scripts, links or events', () => {
		for (const name of ICON_NAMES) {
			const body = ICON_PATHS[name];
			expect(body, `${name} has no script`).not.toMatch(/<script|javascript:|on\w+=|http/i);
			const tags = [...body.matchAll(/<([a-zA-Z]+)/g)].map((m) => m[1]);
			expect(tags.length, `${name} is not empty`).toBeGreaterThan(0);
			for (const tag of tags) {
				expect(['path', 'circle', 'rect'], `${name} <${tag}> is an allowed shape`).toContain(tag);
			}
		}
	});

	it('single-segment subpaths retain a visible stroke instead of relying on fill', () => {
		const number = String.raw`[+-]?(?:\d*\.\d+|\d+\.?\d*)(?:[eE][+-]?\d+)?`;
		const segments = new RegExp(
			String.raw`^(?:\s*[Mm]\s*${number}[\s,]+${number}\s*[hHvV]\s*${number}\s*)+$`
		);
		let checked = 0;
		for (const name of ICON_NAMES) {
			for (const [path] of ICON_PATHS[name].matchAll(/<path\b[^>]*>/g)) {
				const data = path.match(/\bd="([^"]+)"/)?.[1];
				if (!data || !segments.test(data)) continue;
				// A straight segment has no fill area, even when fill is currentColor.
				const stroke = path.match(/\bstroke="([^"]+)"/)?.[1] ?? 'currentColor';
				expect(stroke, `${name} open segments retain the visible inherited stroke`).toBe(
					'currentColor'
				);
				checked++;
			}
		}
		expect(checked).toBeGreaterThanOrEqual(3);
	});
});

describe('icon subset: component', () => {
	it('decorative icons render an aria-hidden svg on the 24 grid', () => {
		const { body } = render(Icon, { props: { name: 'search' } as never });
		expect(body).toMatch(/<svg\b[^>]*viewBox="0 0 24 24"[^>]*>/);
		expect(body).toMatch(/stroke="currentColor"/);
		expect(body).toMatch(/stroke-linecap="round"/);
		expect(body).toMatch(/aria-hidden="true"/);
		expect(body).not.toMatch(/role="img"/);
	});

	it('labelled icons expose role img with the host-provided name', () => {
		const { body } = render(Icon, { props: { name: 'play', label: 'Play' } as never });
		expect(body).toMatch(/role="img"/);
		expect(body).toMatch(/aria-label="Play"/);
		expect(body).not.toMatch(/aria-hidden/);
	});

	it('every stencil renders without escaping through the component', () => {
		for (const name of ICON_NAMES) {
			const { body } = render(Icon, { props: { name } as never });
			expect(body, `${name} renders its stencil`).toContain(ICON_PATHS[name].slice(0, 24));
		}
	});

	it('size prop reaches width and height', () => {
		const { body } = render(Icon, { props: { name: 'x', size: 16 } as never });
		expect(body).toMatch(/width="16"/);
		expect(body).toMatch(/height="16"/);
	});
});

describe('icon subset: budget', () => {
	it('counts comment markers and repeated whitespace inside exported strings', async () => {
		const value = '<!-- visible --> /* literal */' + ' '.repeat(BUDGET_BYTES);
		const source = `export const stencil = ${JSON.stringify(value)};`;
		const expected = new TextEncoder().encode(`export const stencil=${JSON.stringify(value)};`);
		// Minification may escape HTML delimiters, but must not erase string content.
		expect(await scriptBytes(source, 'literal.ts')).toBeGreaterThanOrEqual(expected.byteLength);
	});

	it('counts UTF-8 bytes rather than UTF-16 code units', async () => {
		const ascii = await scriptBytes('export const label = "ab";', 'ascii.ts');
		const unicode = await scriptBytes('export const label = "音楽";', 'unicode.ts');
		expect(unicode - ascii).toBe(4);
	});

	it('includes oversized exported data even when no route imports it', async () => {
		const source = `export const extra = ${JSON.stringify('x'.repeat(BUDGET_BYTES + 1))};`;
		expect(await scriptBytes(source, 'oversized.ts')).toBeGreaterThan(BUDGET_BYTES);
	});

	it('rejects empty script output instead of counting it as zero bytes', async () => {
		await expect(scriptBytes('/* no script */', 'empty.ts')).rejects.toThrow('No compiled script');
	});

	it('added script content stays within 8 KB', async () => {
		const files = ['./icons.ts', './strings.ts', './Icon.svelte'];
		const sizes = await Promise.all(
			files.map(async (f) => [f, await scriptBytes(sources[f], f)] as const)
		);
		const total = sizes.reduce((sum, [, n]) => sum + n, 0);
		const report = sizes.map(([f, n]) => `${f}=${n}B`).join(' ');
		expect(total, `icon set content ${report} total=${total}B <= ${BUDGET_BYTES}B`).toBeLessThanOrEqual(
			BUDGET_BYTES
		);
	});
});
