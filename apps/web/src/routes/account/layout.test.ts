import { createRawSnippet } from 'svelte';
import { render } from 'svelte/server';
import { describe, expect, it, vi } from 'vitest';

/**
 * Acceptance: account layout navigation state and shared styling.
 *
 * The account nav must tell assistive tech which section is current:
 * exactly the link matching the current page URL carries
 * `aria-current="page"`, and switching pages moves that marker. The
 * layout must also use the shared page wrapper (`nt-wrap`, defined with
 * the design tokens in `app.css`) instead of its own hard-coded font
 * stack, so account pages inherit the token font like every other page.
 *
 * The current URL is supplied through both SvelteKit page seams
 * (`$app/state`, as already used by `+error.svelte`, and the legacy
 * `$app/stores` page store) so the test pins the rendered behavior, not
 * which import the implementation chooses. The Builder must not edit
 * this file.
 */

const nav = vi.hoisted(() => ({ pathname: '/account/billing' }));

function currentPage() {
	return {
		url: new URL(`https://example.test${nav.pathname}`),
		params: {},
		route: { id: nav.pathname },
		status: 200,
		error: null,
		data: {},
		state: {},
		form: null
	};
}

vi.mock('$app/state', () => ({
	get page() {
		return currentPage();
	},
	navigating: { current: null },
	updated: { current: false }
}));

vi.mock('$app/stores', () => ({
	page: {
		subscribe(run: (value: ReturnType<typeof currentPage>) => void) {
			run(currentPage());
			return () => {};
		}
	},
	navigating: {
		subscribe(run: (value: null) => void) {
			run(null);
			return () => {};
		}
	},
	updated: {
		subscribe(run: (value: boolean) => void) {
			run(false);
			return () => {};
		},
		check: async () => false
	},
	getStores: () => ({})
}));

import AccountLayout from './+layout.svelte';

const layoutSources = import.meta.glob('./+layout.svelte', {
	query: '?raw',
	import: 'default',
	eager: true
}) as Record<string, string>;

const children = createRawSnippet(() => ({
	render: () => `<p data-testid="layout-child">Account child content</p>`
}));

function renderLayout(pathname: string): string {
	nav.pathname = pathname;
	// The layout reads only `children`; SvelteKit's generated LayoutProps
	// also requires route `data`/`params`, which SSR rendering never reads.
	return render(AccountLayout, { props: { children } as never }).body;
}

/** Opening `<a>` tag for an href, or null when the link is absent. */
function linkTag(body: string, href: string): string | null {
	for (const tag of body.matchAll(/<a\b[^>]*>/g)) {
		if (tag[0].includes(`href="${href}"`)) return tag[0];
	}
	return null;
}

describe('account layout navigation', () => {
	it('marks Billing as the current page on /account/billing', () => {
		const body = renderLayout('/account/billing');

		expect(linkTag(body, '/account/billing')).toContain('aria-current="page"');
		expect(linkTag(body, '/account/security')).not.toContain('aria-current');
	});

	it('marks Security as the current page on /account/security', () => {
		const body = renderLayout('/account/security');

		expect(linkTag(body, '/account/security')).toContain('aria-current="page"');
		expect(linkTag(body, '/account/billing')).not.toContain('aria-current');
	});

	it('marks no link current outside the account sections', () => {
		const body = renderLayout('/account');

		expect(linkTag(body, '/account/billing')).not.toContain('aria-current');
		expect(linkTag(body, '/account/security')).not.toContain('aria-current');
	});

	it('keeps both account links and still renders the page content', () => {
		const body = renderLayout('/account/billing');

		expect(body).toMatch(/<nav\b[^>]*aria-label="Account"/);
		expect(linkTag(body, '/account/billing')).not.toBeNull();
		expect(linkTag(body, '/account/security')).not.toBeNull();
		expect(body).toContain('Account child content');
	});
});

describe('account layout shared styling', () => {
	it('renders inside the shared nt-wrap wrapper', () => {
		const body = renderLayout('/account/billing');

		expect(body).toMatch(/class="[^"]*\bnt-wrap\b/);
	});

	it('carries no hard-coded system font stack', () => {
		const source = Object.values(layoutSources)[0] ?? '';

		expect(source.length).toBeGreaterThan(0);
		expect(source).not.toMatch(/system-ui/);
	});
});
