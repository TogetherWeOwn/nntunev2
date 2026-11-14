import { createRawSnippet } from 'svelte';
import { render } from 'svelte/server';
import { describe, expect, it, vi } from 'vitest';

const page = vi.hoisted(() => ({ url: new URL('https://nntune.test/home') }));
vi.mock('$app/state', () => ({ page }));
vi.mock('$app/navigation', () => ({ afterNavigate: () => {} }));

import Layout from './+layout.svelte';

const children = createRawSnippet(() => ({
	render: () => '<main><h1>Shell child</h1></main>'
}));

function markup(path: string) {
	page.url = new URL(path, 'https://nntune.test');
	return render(Layout, { props: { children } as never }).body;
}

function appearanceControl(body: string) {
	const controls = body.match(/<select\b[^>]*id="nt-appearance-select"[^>]*>[\s\S]*?<\/select>/g) ?? [];
	expect(controls).toHaveLength(1);
	for (const [value, label] of [['system', 'System'], ['light', 'Light'], ['dark', 'Dark']]) {
		expect(controls[0]).toMatch(new RegExp(`<option\\b[^>]*value="${value}"[^>]*>\\s*${label}\\s*</option>`));
	}
	expect(controls[0]).toMatch(/<option\b(?=[^>]*value="system")(?=[^>]*\bselected(?:\s|=|>))[^>]*>/);
	expect(body.match(/<label\b[^>]*for="nt-appearance-select"/g)).toHaveLength(1);
}

describe('shared shell markup', () => {
	it.each(['/', '/login', '/auth/magic'])('keeps one appearance control and no app navigation at %s', (path) => {
		const body = markup(path);
		appearanceControl(body);
		expect(body).toContain('Shell child');
		expect(body).not.toContain('aria-label="Primary"');
		expect(body).not.toContain('data-create-anchor=');
	});

	it.each([
		['/home', '/home'], ['/search?q=one', '/search'],
		['/library/import', '/library'], ['/library2', null], ['/account/billing', null]
	])('keeps identical rail and tab destinations at %s', (path, current) => {
		const body = markup(path!);
		appearanceControl(body);
		const navs = body.match(/<nav\b[^>]*aria-label="Primary"[^>]*>[\s\S]*?<\/nav>/g) ?? [];
		expect(navs).toHaveLength(2);
		for (const nav of navs) {
			const links = nav.match(/<a\b[^>]*>[\s\S]*?<\/a>/g) ?? [];
			expect(links).toHaveLength(3);
			for (const [index, href] of ['/home', '/search', '/library'].entries()) {
				expect(links[index]).toContain(`href="${href}"`);
				expect(links[index].includes('aria-current="page"')).toBe(href === current);
			}
		}
		for (const anchor of ['rail', 'topbar']) {
			expect(body).not.toContain(`data-create-anchor="${anchor}"`);
			expect(body).not.toContain(`aria-controls="nt-create-menu-${anchor}"`);
			expect(body).not.toContain(`id="nt-create-menu-${anchor}"`);
		}
		expect(body).not.toContain('role="menuitem"');
		expect(body).toContain('Shell child');
	});
});
