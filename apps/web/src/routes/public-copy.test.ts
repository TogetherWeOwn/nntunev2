import { render } from 'svelte/server';
import { describe, expect, it } from 'vitest';
import Home from './+page.svelte';
import Signup from './signup/+page.svelte';
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

	it('describes account creation and plan selection in signup metadata without promising playback', () => {
		const { head } = render(Signup);
		const description = head.match(/<meta name="description" content="([^"]*)"/u)?.[1];
		expect(description).toMatch(/create your NNTune account/i);
		expect(description).toMatch(/choose an account plan/i);
		expect(description).not.toMatch(/listen|stream|provider|Usenet|BYOP/i);
	});

	it('keeps the install manifest aligned with the account-plan flow', async () => {
		const response = await GET({} as never);
		expectAccountPlanCopy((await response.json()).description);
	});
});
