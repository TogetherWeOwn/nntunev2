import { render } from 'svelte/server';
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
 * Budget method: comment-stripped, whitespace-collapsed source bytes of
 * the three icon files. Production minification only shrinks further
 * (types erased, strings compressed), and tree-shaking drops whatever
 * no route imports — so this is the fail-closed upper bound.
 */

const BUDGET_BYTES = 8192;

function contentBytes(path: string): number {
	const src = sources[path];
	return src
		.replace(/\/\*[\s\S]*?\*\//g, '')
		.replace(/<!--[\s\S]*?-->/g, '')
		.replace(/\s+/g, ' ')
		.trim().length;
}

describe('icon subset: manifest', () => {
	it('ships the R1/R2 set: 49 names, each with stencil data', () => {
		expect(ICON_NAMES).toHaveLength(49);
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
});

describe('icon subset: component', () => {
	it('decorative icons render an aria-hidden svg on the 24 grid', () => {
		const { body } = render(Icon, { props: { name: 'search' } as never });
		expect(body).toMatch(/<svg\b[^>]*viewBox="0 0 24 24"[^>]*>/);
		expect(body).toMatch(/stroke="currentColor"/);
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
	it('added script content stays within 8 KB', () => {
		const files = ['./icons.ts', './strings.ts', './Icon.svelte'];
		const sizes = files.map((f) => [f, contentBytes(f)] as const);
		const total = sizes.reduce((sum, [, n]) => sum + n, 0);
		const report = sizes.map(([f, n]) => `${f}=${n}B`).join(' ');
		expect(total, `icon set content ${report} total=${total}B <= ${BUDGET_BYTES}B`).toBeLessThanOrEqual(
			BUDGET_BYTES
		);
	});
});
