import { createRawSnippet } from 'svelte';
import { render } from 'svelte/server';
import { afterEach, describe, expect, it, vi } from 'vitest';

vi.mock('$app/navigation', () => ({ afterNavigate: () => {} }));
// Direct SSR rendering has no Kit request context. Supply page state at that
// boundary while keeping the actual root layout and its children under test.
vi.mock('$app/state', () => ({
	page: {
		data: {},
		error: null,
		form: null,
		params: {},
		route: { id: '/library' },
		state: {},
		status: 200,
		url: new URL('https://nntune.test/library')
	}
}));

import Layout from './+layout.svelte';

/** Protected acceptance: render the real existing layout, not a new stub/module.
 * Client preference validation and lifecycle are exercised by the paired browser
 * suites. No new production export is required. See tests/appearance.acceptance.md.
 */
const children = createRawSnippet(() => ({
	render: () => '<main><h1>Appearance acceptance child</h1></main>'
}));

function renderLayout() {
	return render(Layout, { props: { children } as never });
}

afterEach(() => vi.unstubAllGlobals());

describe('appearance SSR acceptance', () => {
	it('APP-SSR-01: offers the three native choices with System selected on the server', () => {
		const { body } = renderLayout();
		const select = body.match(/<select\b[^>]*>[\s\S]*?<\/select>/)?.[0] ?? '';
		expect(select, 'the existing layout must server-render the appearance selector').not.toBe('');
		for (const [value, label] of [['system', 'System'], ['light', 'Light'], ['dark', 'Dark']]) {
			expect(select).toMatch(new RegExp(`<option\\b[^>]*value="${value}"[^>]*>\\s*${label}\\s*</option>`));
		}
		expect(select).toMatch(/<option\b(?=[^>]*value="system")(?=[^>]*\bselected(?:\s|=|>))[^>]*>/);
	});

	it('APP-SSR-02: renders without reading browser globals or device storage', () => {
		// Model Node SSR (no window/document); trap an unguarded direct storage
		// access as well. Browser-global presence is not fabricated on the server.
		vi.stubGlobal('window', undefined);
		vi.stubGlobal('document', undefined);
		vi.stubGlobal('localStorage', new Proxy({}, { get() { throw new Error('storage access during SSR'); } }));
		expect(renderLayout().body).toContain('Appearance acceptance child');
	});

	it('APP-SSR-03: keeps the existing page content and PWA metadata', () => {
		const result = renderLayout();
		expect(result.body).toContain('<h1>Appearance acceptance child</h1>');
		expect(result.head.match(/rel="manifest"/g)).toHaveLength(1);
		expect(result.head).toMatch(/href="\/manifest.webmanifest"/);
		expect(result.head).toMatch(/name="theme-color"\s+content="#0b0d12"/);
	});
});
