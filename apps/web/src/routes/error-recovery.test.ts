import { render } from 'svelte/server';
import { describe, expect, it, vi } from 'vitest';

/**
 * Acceptance: error-page recovery for non-404 errors.
 *
 * The non-404 copy already says "Try again" but offers no way to do so.
 * For non-404 statuses the page must offer a working Reload action (a
 * button whose handler reloads / re-runs the route) and must draw
 * attention to the failure — focus moved to the heading
 * (`tabindex="-1"` / autofocus on the `<h1>`) or an announcement via
 * `role="alert"`. The 404 page keeps its current navigation (library
 * and sign-in links, no Reload). No raw error text from `page.error`
 * is ever rendered, at any status.
 *
 * Status and error are supplied through both SvelteKit page seams
 * (`$app/state`, which `+error.svelte` already reads, and the legacy
 * `$app/stores` page store) so the test pins rendered behavior, not
 * the chosen import. The Builder must not edit this file.
 */

const RAW_ERROR_SENTINEL = 'RAW-ERROR-SENTINEL-7f3a-do-not-render';

const pageState = vi.hoisted(() => ({
	status: 500,
	error: { message: 'RAW-ERROR-SENTINEL-7f3a-do-not-render' } as { message: string } | null
}));

function currentPage() {
	return {
		url: new URL('https://example.test/some/page'),
		params: {},
		route: { id: '/some/page' },
		status: pageState.status,
		error: pageState.error,
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

import ErrorPage from './+error.svelte';

const errorSources = import.meta.glob('./+error.svelte', {
	query: '?raw',
	import: 'default',
	eager: true
}) as Record<string, string>;

function renderError(status: number): string {
	pageState.status = status;
	pageState.error = { message: RAW_ERROR_SENTINEL };
	return render(ErrorPage).body;
}

/** Text of every rendered `<button>`, tags stripped. */
function buttonTexts(body: string): string[] {
	return [...body.matchAll(/<button\b[^>]*>([\s\S]*?)<\/button>/g)].map((match) =>
		(match[1] ?? '')
			.replace(/<[^>]*>/g, ' ')
			.replace(/\s+/g, ' ')
			.trim()
	);
}

function hasReloadButton(body: string): boolean {
	return buttonTexts(body).some((text) => /reload|try again|retry/i.test(text));
}

/** Opening `<h1>` tag, or null. */
function headingTag(body: string): string | null {
	return body.match(/<h1\b[^>]*>/)?.[0] ?? null;
}

describe('error page recovery (non-404)', () => {
	it('offers a Reload action matching the Try again copy', () => {
		const body = renderError(500);

		expect(body).toMatch(/Something went wrong/);
		expect(hasReloadButton(body)).toBe(true);
	});

	it('wires the Reload action to actually reload or re-run the route', () => {
		const source = Object.values(errorSources)[0] ?? '';

		expect(source.length).toBeGreaterThan(0);
		// Any real reload mechanism counts; a dead button does not.
		expect(source).toMatch(
			/location\s*\.\s*reload\s*\(|invalidateAll\s*\(|location\s*\.\s*(?:href|assign|replace)\b/
		);
	});

	it('draws attention to the failure via heading focus or role="alert"', () => {
		const body = renderError(503);
		const h1 = headingTag(body) ?? '';
		const headingFocusable = /tabindex="-1"/.test(h1) || /\bautofocus\b/.test(h1);
		const announced = /role="alert"/.test(body);

		expect(headingFocusable || announced).toBe(true);
	});

	it('keeps the library and sign-in navigation alongside Reload', () => {
		const body = renderError(500);

		expect(body).toMatch(/href="\/library"/);
		expect(body).toMatch(/href="\/login"/);
	});
});

describe('error page (404) keeps current navigation', () => {
	it('renders the 404 heading with library and sign-in links and no Reload', () => {
		const body = renderError(404);

		expect(body).toMatch(/<h1[^>]*>[\s\S]*?404[\s\S]*?<\/h1>/i);
		expect(body).toMatch(/href="\/library"/);
		expect(body).toMatch(/href="\/login"/);
		expect(hasReloadButton(body)).toBe(false);
	});
});

describe('error page never renders raw error text', () => {
	it.each([404, 500, 503])('does not render page.error at status %i', (status) => {
		const body = renderError(status);

		expect(body).not.toContain(RAW_ERROR_SENTINEL);
	});
});
