import { createRawSnippet } from 'svelte';
import { render } from 'svelte/server';
import { describe, expect, it, vi } from 'vitest';

const nav = vi.hoisted(() => ({ pathname: '/account/provider' }));

vi.mock('$app/state', () => ({
	get page() {
		return { url: new URL(`https://example.test${nav.pathname}`) };
	}
}));

import AccountLayout from './+layout.svelte';

const children = createRawSnippet(() => ({ render: () => '<p>Account content</p>' }));

// Supplemental integration coverage for the Provider link added on the base
// branch; the original paired acceptance file stays unchanged.
describe('account navigation with Provider', () => {
	for (const section of ['billing', 'security', 'provider']) {
		it(`marks only ${section} current and preserves all three links`, () => {
			nav.pathname = `/account/${section}?from=account`;
			const body = render(AccountLayout, { props: { children } as never }).body;
			const links = [...body.matchAll(/<a\b[^>]*>/g)].map((match) => match[0]);

			for (const target of ['billing', 'security', 'provider']) {
				const link = links.find((tag) => tag.includes(`href="/account/${target}"`));
				expect(link).toBeDefined();
				if (target === section) expect(link).toContain('aria-current="page"');
				else expect(link).not.toContain('aria-current');
			}
			expect(body.match(/aria-current="page"/g)).toHaveLength(1);
		});
	}

	it('does not mark Provider current for a similarly prefixed unknown route', () => {
		nav.pathname = '/account/provider-other';
		const body = render(AccountLayout, { props: { children } as never }).body;
		expect(body).not.toContain('aria-current');
	});
});
