import { createRawSnippet } from 'svelte';
import { render } from 'svelte/server';
import { describe, expect, it, vi } from 'vitest';

const nav = vi.hoisted(() => ({ pathname: '/account/security' }));

vi.mock('$app/state', () => ({
	get page() {
		return { url: new URL(`https://example.test${nav.pathname}`) };
	}
}));

import AccountLayout from './+layout.svelte';

const children = createRawSnippet(() => ({ render: () => '<p>Account content</p>' }));

// The user provider page is retired, so the account shell links only to
// billing and security; the original paired acceptance file stays unchanged.
describe('account navigation without Provider', () => {
	for (const section of ['billing', 'security']) {
		it(`marks only ${section} current and preserves both links`, () => {
			nav.pathname = `/account/${section}?from=account`;
			const body = render(AccountLayout, { props: { children } as never }).body;
			const links = [...body.matchAll(/<a\b[^>]*>/g)].map((match) => match[0]);

			for (const target of ['billing', 'security']) {
				const link = links.find((tag) => tag.includes(`href="/account/${target}"`));
				expect(link).toBeDefined();
				if (target === section) expect(link).toContain('aria-current="page"');
				else expect(link).not.toContain('aria-current');
			}
			expect(body.match(/aria-current="page"/g)).toHaveLength(1);
			expect(body).not.toContain('href="/account/provider"');
		});
	}

	it('does not mark anything current for a similarly prefixed unknown route', () => {
		nav.pathname = '/account/other';
		const body = render(AccountLayout, { props: { children } as never }).body;
		expect(body).not.toContain('aria-current');
	});
});
