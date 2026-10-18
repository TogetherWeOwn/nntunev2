import { describe, expect, it } from 'vitest';

/**
 * Branded 404 contract (test-first): unknown routes must render
 * the NNTune `+error.svelte` page — branded, with a safe-navigation CTA to
 * `/library` and/or `/login` — never a stack trace or default SvelteKit
 * chrome, and never leaking secret, hostname, or binding material.
 *
 * This spec reads the route source through an eager raw glob instead of a
 * static import (no `node:` imports, no tsconfig change), so `svelte-check`
 * stays green while the page is unimplemented; the assertions below fail
 * (missing file) until the Builder lands `+error.svelte` on this branch.
 * The Builder must not edit this file.
 */
const sources = import.meta.glob('./+error.svelte', {
	query: '?raw',
	import: 'default',
	eager: true
}) as Record<string, string>;

function readErrorPage(): string {
	const source = Object.values(sources)[0];
	if (typeof source !== 'string' || source.length === 0) {
		throw new Error(
			'missing branded error page: src/routes/+error.svelte does not exist yet — ' +
				'unknown routes have no NNTune 404. The Builder implements it on this branch.'
		);
	}
	return source;
}

describe('branded error page (unknown routes)', () => {
	it('renders a branded 404 with a safe-navigation CTA, not SvelteKit chrome', () => {
		const source = readErrorPage();

		// Branded: NNTune identity plus an explicit 404 marker in a heading.
		expect(source).toMatch(/NNTune/);
		expect(source).toMatch(/<h1[^>]*>[\s\S]*?404[\s\S]*?<\/h1>/i);
		// Branded styling hooks (design tokens), not unstyled default output.
		expect(source).toMatch(/nt-(wrap|button)|var\(--nt-/);
		// Safe navigation: at least one CTA back into known routes.
		expect(source).toMatch(/href="\/(library|login)"/);
	});

	it('never renders a stack trace or default SvelteKit chrome', () => {
		const source = readErrorPage();

		expect(source).not.toMatch(/stack\s*trace/i);
		expect(source).not.toMatch(/\.stack\b/);
		expect(source).not.toMatch(/sveltekit/i);
		expect(source).not.toMatch(/__sveltekit/i);
		expect(source).not.toMatch(/console\.(log|trace|dir)\s*\(/);
	});

	it('leaks no secret, hostname, or binding material', () => {
		const source = readErrorPage();

		// Private env / platform bindings must never be referenced.
		expect(source).not.toMatch(/\$env\/(dynamic|static)\/private/);
		expect(source).not.toMatch(/platform\.env\b/);
		expect(source).not.toMatch(/secret/i);
		expect(source).not.toMatch(/api[_-]?key/i);
		expect(source).not.toMatch(/private[_-]?key/i);
		expect(source).not.toMatch(/bearer/i);
		expect(source).not.toMatch(/binding/i);
		// Hostnames / origins must never be rendered or composed.
		expect(source).not.toMatch(/hostname/i);
		expect(source).not.toMatch(/\.host\b/);
		expect(source).not.toMatch(/\.origin\b/);
		expect(source).not.toMatch(/workers\.dev/i);
		// Cloudflare binding prefixes must never appear.
		expect(source).not.toMatch(/\b(KV_|D1_|R2_)\w*/);
	});
});
