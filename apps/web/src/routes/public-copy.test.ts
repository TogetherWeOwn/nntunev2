import { render } from 'svelte/server';
import { describe, expect, it } from 'vitest';
import Home from './+page.svelte';
import { GET } from './manifest.webmanifest/+server';

function expectAccountPlanCopy(copy: string) {
	expect(copy).toMatch(/sign up/i);
	expect(copy).toMatch(/account plan/i);
	expect(copy).not.toMatch(/provider|Usenet|BYOP/i);
	expect(copy).toMatch(
		/search, streaming, library management and offline caching are\s+not wired yet/i
	);
}

describe('public account-plan copy', () => {
	it('keeps the home metadata and paragraph honest without promising provider setup', () => {
		const { head, body } = render(Home);
		expectAccountPlanCopy(head);
		expectAccountPlanCopy(body);
		expect(body).toContain('href="/signup"');
		expect(body).toContain('href="/pricing"');
	});

	it('keeps the install manifest aligned with the account-plan flow', async () => {
		const response = await GET({} as never);
		expectAccountPlanCopy((await response.json()).description);
	});
});
