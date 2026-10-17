import { describe, expect, it } from 'vitest';
import { load } from './+page';
import type { PageData } from './$types';

function mockFetch401(): typeof fetch {
	return (async () =>
		new Response(JSON.stringify({ error: 'unauthorized' }), {
			status: 401
		})) as typeof fetch;
}

describe('signed-out billing view', () => {
	it('sends the Sign in link to /login and offers no checkout or portal action', async () => {
		const { render } = await import('svelte/server');
		const { default: BillingPage } = await import('./+page.svelte');

		const data = await load({ fetch: mockFetch401() } as never);
		expect(data).toBeDefined();
		expect(data!.signedOut).toBe(true);

		const { body } = render(BillingPage, { props: { data: data as PageData } });

		expect(body).toContain('href="/login"');
		expect(body).toMatch(/href="\/login"[^>]*>\s*Sign in\s*<\/a>/);
		expect(body).not.toMatch(/href="\/signup"[^>]*>\s*Sign in\s*<\/a>/);
		expect(body).not.toContain('Manage billing');
		expect(body).not.toContain('Upgrade to');
	});
});
