import { describe, expect, it } from 'vitest';
import { render } from 'svelte/server';
import SecurityPage from './+page.svelte';

describe('account security page', () => {
	it('renders separate controls for signing out locally and everywhere', () => {
		const { body } = render(SecurityPage);

		expect(body).toMatch(/<h1[^>]*>Account security<\/h1>/i);
		expect(body).toMatch(/<button[^>]*>[\s\S]*?Sign out everywhere[\s\S]*?<\/button>/i);
		expect(body).toMatch(/<button[^>]*>[\s\S]*?Sign out[\s\S]*?<\/button>/i);
	});
});
