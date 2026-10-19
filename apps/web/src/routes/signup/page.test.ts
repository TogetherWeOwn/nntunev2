import { describe, expect, it } from 'vitest';
import { render } from 'svelte/server';
import SignupPage from './+page.svelte';

/**
 * Pins the signup wrong/expired-code rendering: the `code-error` alert
 * plus the Start over button, and the Start over return to a fresh-code
 * state. Invite-field behavior on retry is recorded as implemented (no
 * redesign).
 *
 * SSR `render` only produces the initial (`idle`) state — the code-error
 * alert appears after a client-side finish failure — so the interactive
 * states are pinned against the route source (same raw-glob approach as
 * `src/routes/error.test.ts`; no `node:` imports, so `svelte-check` stays
 * green).
 */
const sources = import.meta.glob('./+page.svelte', {
	query: '?raw',
	import: 'default',
	eager: true
}) as Record<string, string>;

function readSignupPage(): string {
	const source = Object.values(sources)[0];
	if (typeof source !== 'string' || source.length === 0) {
		throw new Error(
			'missing signup page source: src/routes/signup/+page.svelte was not found by the raw glob'
		);
	}
	return source;
}

describe('signup page initial state', () => {
	it('renders the email step with no alert', () => {
		const { body } = render(SignupPage);

		expect(body).toMatch(/<h1[^>]*>Get started<\/h1>/);
		expect(body).toContain('data-testid="signup-email"');
		expect(body).toContain('data-testid="signup-submit"');
		expect(body).not.toContain('role="alert"');
	});
});

describe('signup code-error and start-over rendering', () => {
	it('renders the code-error alert plus the Start over button on a wrong/expired code', () => {
		const source = readSignupPage();

		// The generic finish catch lands on `code-error` (account creation
		// never says which step failed).
		expect(source).toMatch(/status\s*=\s*'code-error'/);
		// Scope the alert, copy, and button to the captured code-error
		// branch so a role change (alert -> status, matching the later
		// signup-error alert) or moving the button out of the branch turns
		// the spec red.
		const codeErrorBlock = source.match(
			/\{#if status === 'code-error'\}([\s\S]*?)\{\/if\}/
		)?.[1];
		expect(codeErrorBlock, "expected a {#if status === 'code-error'} block").toBeDefined();
		const block = codeErrorBlock!;
		// The code-error branch renders an alert with the exact current copy.
		expect(block).toMatch(/role="alert"/);
		expect(block).toMatch(/That didn't work/);
		expect(block).toMatch(/start over to get a fresh code/i);
		// ... plus a Start over button wired to the reset, inside the branch.
		expect(block).toMatch(
			/<button[^>]*onclick=\{startOver\}[^>]*>[\s\S]*?Start over[\s\S]*?<\/button>/
		);
		// The outer code step still renders on code-error and gates the
		// retry form: both statuses must appear inside the SAME {#if ...}
		// tag (no `}` allowed between), so dropping code-error from the
		// outer condition turns the spec red instead of matching the inner
		// alert condition.
		expect(source).toMatch(
			/\{#if status === 'code-sent'[^}]*\|\| status === 'code-error'[^}]*\}[\s\S]*?<form onsubmit=\{finishAccount\}[\s\S]*?name="magic_code"/
		);
	});

	it('Start over returns to a fresh-code state', () => {
		const source = readSignupPage();

		// startOver clears the single-use attempt: pending challenge, both
		// typed codes, and back to the email step.
		const reset = source.match(/function startOver\(\) \{([\s\S]*?)\n\t\}/);
		expect(reset, 'expected a startOver() reset function').not.toBeNull();
		const resetBody = reset![1];
		expect(resetBody).toMatch(/pending\s*=\s*null/);
		expect(resetBody).toMatch(/magicCode\s*=\s*''/);
		expect(resetBody).toMatch(/inviteCode\s*=\s*''/);
		expect(resetBody).toMatch(/status\s*=\s*'idle'/);
		// Idle renders the email step (the SSR test above proves the markup).
		expect(source).toMatch(/\{:else\}[\s\S]*?data-testid="signup-email"/);
	});

	it('records invite-field behavior on retry as implemented (no redesign)', () => {
		const source = readSignupPage();

		// Implemented behavior: a code-error retry keeps the typed codes in
		// place — the generic catch only flips status, so the invite field
		// stays bound and is re-sent with the retry. Only Start over clears it.
		expect(source).toContain('name="invite_code"');
		expect(source).toMatch(/bind:value=\{inviteCode\}/);
		expect(source).toMatch(/inviteCode\.trim\(\)\.length > 0 \? \{ inviteCode:/);
		// The only resets of the typed codes are inside startOver().
		expect(source.match(/magicCode\s*=\s*''/g)).toHaveLength(1);
		expect(source.match(/inviteCode\s*=\s*''/g)).toHaveLength(1);
	});
});
