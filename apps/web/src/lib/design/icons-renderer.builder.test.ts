import { compile } from 'svelte/compiler';
import { render } from 'svelte/server';
import { minify } from 'terser';
import ts from 'typescript';
import { describe, expect, it } from 'vitest';
import Icon from './Icon.svelte';
import IconSvg from './IconSvg.svelte';
import { FAVORITE_ICON_PATHS, ICON_NAMES, ICON_PATHS, TRANSPORT_ICON_PATHS } from './icons.js';

const sources = import.meta.glob('./{icons.ts,strings.ts,Icon.svelte,IconSvg.svelte}', {
	query: '?raw', import: 'default', eager: true
}) as Record<string, string>;

function svg(body: string): string {
	const match = body.match(/<svg\b[\s\S]*?<\/svg>/)?.[0];
	if (!match) throw new Error('Missing SVG');
	let markup: string = match;
	while (markup.includes('<!--')) {
		const next: string = markup.replace(/<!--[\s\S]*?-->/g, '');
		if (next === markup) throw new Error('Unterminated SVG comment');
		markup = next;
	}
	return markup;
}

describe('shared kit SVG renderer', () => {
	it('removes comments formed by earlier replacements and rejects unterminated comments', () => {
		expect(svg('<svg><!<!-- -->--hidden--><path /></svg>')).toBe('<svg><path /></svg>');
		expect(() => svg('<svg><!<!-- -->--unterminated</svg>')).toThrow('Unterminated SVG comment');
	});

	it('retains every delivered stencil byte and manifest order', async () => {
		const data = JSON.stringify(ICON_NAMES.map((name) => [name, ICON_PATHS[name]]));
		const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(data));
		const hex = Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, '0')).join('');
		expect(hex).toBe('c2e485c7fa316c9a1244426dcbdfb2776e341c88d57d29bf12820b2a101ec5a9');
	});

	it('keeps the same decorative SVG attributes and shapes for all named icons', () => {
		for (const name of ICON_NAMES) {
			const expected = `<svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" focusable="false" aria-hidden="true">${ICON_PATHS[name]}</svg>`;
			expect(svg(render(Icon, { props: { name } }).body), name).toBe(expected);
			expect(svg(render(IconSvg, { props: { path: ICON_PATHS[name] } }).body), name).toBe(expected);
		}
	});

	it.each([16, '2em'])('preserves labelled semantics, escaping and size %s', (size) => {
		const props = { label: 'Play & <pause>', size };
		const body = svg(render(IconSvg, { props: { path: TRANSPORT_ICON_PATHS.play, ...props } }).body);
		expect(body).toBe(svg(render(Icon, { props: { name: 'play', ...props } }).body));
		expect(body).toContain('role="img"');
		expect(body).toContain('aria-label="Play &amp; &lt;pause>"');
		expect(body).toContain(`width="${size}" height="${size}"`);
		expect(body).not.toContain('aria-hidden');
	});

	it('exposes only the existing ten transport stencils through the narrow import', () => {
		expect(Object.keys(TRANSPORT_ICON_PATHS)).toEqual([
			'play', 'pause', 'skip-back', 'skip-forward', 'restart', 'shuffle', 'repeat', 'repeat-1', 'volume-2', 'volume-x'
		]);
		for (const [name, path] of Object.entries(TRANSPORT_ICON_PATHS)) {
			expect(path).toBe(ICON_PATHS[name as keyof typeof ICON_PATHS]);
		}
	});

	it('exposes the two delivered heart stencils without importing the full manifest', () => {
		expect(Object.keys(FAVORITE_ICON_PATHS)).toEqual(['heart', 'heart-filled']);
		for (const name of ['heart', 'heart-filled'] as const) {
			expect(FAVORITE_ICON_PATHS[name]).toBe(ICON_PATHS[name]);
			expect(svg(render(IconSvg, { props: { path: FAVORITE_ICON_PATHS[name] } }).body))
			.toBe(svg(render(Icon, { props: { name } }).body));
		}
	});

	it('counts the extracted renderer as well as all kit exports inside the unchanged 8 KiB cap', async () => {
		const sizes = await Promise.all(Object.entries(sources).map(async ([filename, source]) => {
			const code = filename.endsWith('.svelte')
				? compile(source, { filename, generate: 'client', dev: false }).js.code
				: ts.transpileModule(source, {
					compilerOptions: { target: ts.ScriptTarget.ESNext, module: ts.ModuleKind.ESNext }
				}).outputText;
			const result = await minify(code, { module: true, format: { comments: false } });
			if (!result.code) throw new Error(`Missing script for ${filename}`);
			return new TextEncoder().encode(result.code).byteLength;
		}));
		expect(sizes).toHaveLength(4);
		expect(sizes.reduce((sum, size) => sum + size, 0)).toBeLessThanOrEqual(8192);
	});
});
