import { describe, expect, it } from 'vitest';
import { render } from 'svelte/server';
import LoginPage from './+page.svelte';

/**
 * Acceptance (component): an in-flight passkey sign-in must never be
 * flagged as failed, and an empty email must never be flagged as failed
 * either.
 *
 * The pre-fix `signInWithPasskey` guard combined both cases:
 * `if (passkeyStatus === 'working' || email.trim().length === 0)` set
 * `passkeyStatus = 'error'`. So re-clicking the (still enabled) button
 * while a ceremony was in flight flipped the page to the generic
 * "Sign-in failed" alert even though the first ceremony was still
 * running, and clicking with no email blamed the sign-in instead of
 * asking for the email.
 *
 * Required behavior:
 * - re-click while `working` is a no-op: the handler returns without
 *   touching the status, the button is disabled, and a working status
 *   (`role="status"`, this page's idiom) is shown;
 * - empty/whitespace email yields an inline prompt asking for the
 *   email (a status/alert naming the email), never the `'error'` state;
 * - genuine failures still land on `'error'` with the exact current
 *   generic copy, and the dismissed-ceremony copy is untouched.
 *
 * SSR `render` only produces the initial (`idle`) state, so the
 * interactive states are pinned against the route source (same
 * raw-glob approach as `src/routes/signup/page.test.ts`).
 * Browser-level behavior is pinned in `apps/web/tests/passkey-in-flight.spec.ts`.
 */
const sources = import.meta.glob('./+page.svelte', {
	query: '?raw',
	import: 'default',
	eager: true
}) as Record<string, string>;

const LOGIN_GENERIC = 'Sign-in failed. Try again or use an email link.';

function readLoginPage(): string {
	const source = Object.values(sources)[0];
	if (typeof source !== 'string' || source.length === 0) {
		throw new Error(
			'missing login page source: src/routes/login/+page.svelte was not found by the raw glob'
		);
	}
	return source;
}

/**
 * The part of `signInWithPasskey` before the ceremony actually starts
 * (`passkeyStatus = 'working'`). Every early return lives here; no
 * early return may flag the attempt as `'error'`.
 */
function handlerPreamble(source: string): string {
	const start = source.indexOf('async function signInWithPasskey()');
	expect(start, 'expected a signInWithPasskey handler').toBeGreaterThanOrEqual(0);
	const rest = source.slice(start);
	const workingAt = rest.search(/passkeyStatus\s*=\s*'working'/);
	expect(workingAt, "expected the handler to set passkeyStatus = 'working'").toBeGreaterThan(0);
	return rest.slice(0, workingAt);
}

describe('login page initial state', () => {
	it('renders the passkey button enabled with no alert', () => {
		const { body } = render(LoginPage);

		expect(body).toMatch(/<h1[^>]*>Sign in<\/h1>/);
		expect(body).toContain('Sign in with passkey');
		expect(body).not.toContain('role="alert"');
		expect(body).not.toContain(LOGIN_GENERIC);
	});
});

describe('login passkey in-flight re-click', () => {
	it('re-click while working returns without flagging the attempt as failed', () => {
		const preamble = handlerPreamble(readLoginPage());

		// A standalone working guard that just returns (compare
		// `sendMagicLink`'s `if (status === 'sending') return;`).
		expect(preamble).toMatch(
			/if\s*\(\s*passkeyStatus\s*===\s*'working'\s*\)\s*(?:\{\s*)?return\s*;/
		);
		// The working case must not be combined into a guard that can
		// reach the error state, and no early return may set 'error'.
		expect(preamble).not.toMatch(/passkeyStatus\s*===\s*'working'\s*\|\|/);
		expect(preamble).not.toMatch(/passkeyStatus\s*=\s*'error'/);
	});

	it('disables the passkey button and shows a working status while working', () => {
		const source = readLoginPage();

		const button = source.match(/<button[^>]*onclick=\{signInWithPasskey\}[^>]*>/)?.[0];
		expect(button, 'expected the passkey button wired to signInWithPasskey').toBeDefined();
		expect(button!).toMatch(/disabled=\{[^}]*passkeyStatus\s*===\s*'working'[^}]*\}/);

		const workingBlock = source.match(
			/\{(?:#if|:else if) passkeyStatus === 'working'\}([\s\S]*?)(?=\{:else|\{\/if\})/
		)?.[1];
		expect(
			workingBlock,
			"expected a rendered branch for passkeyStatus === 'working'"
		).toBeDefined();
		expect(workingBlock!).toMatch(/role="status"/);
		expect(workingBlock!).not.toContain(LOGIN_GENERIC);
	});
});

describe('login passkey empty email', () => {
	it('yields an inline email prompt, never the error state', () => {
		const source = readLoginPage();
		const preamble = handlerPreamble(source);

		// The empty-email guard exists before the ceremony starts…
		const guardAt = preamble.search(/email\.trim\(\)(?:\.length\s*===\s*0)?/);
		expect(guardAt, 'expected an empty-email guard before the ceremony').toBeGreaterThanOrEqual(
			0
		);
		// …and the state it lands on is a dedicated prompt state — any
		// name is fine, but it may not be 'error' (or 'working').
		const landed = preamble
			.slice(guardAt)
			.match(/passkeyStatus\s*=\s*'([a-z][a-z-]*)'\s*;\s*return/)?.[1];
		expect(landed, 'expected the empty-email guard to set a prompt status and return').toBeDefined();
		expect(landed).not.toBe('error');
		expect(landed).not.toBe('working');

		// That state renders an inline prompt naming the email, not the
		// generic failure copy.
		const promptBlock = source.match(
			new RegExp(
				`\\{(?:#if|:else if) passkeyStatus === '${landed}'\\}([\\s\\S]*?)(?=\\{:else|\\{\\/if\\})`
			)
		)?.[1];
		expect(promptBlock, `expected a rendered branch for passkeyStatus === '${landed}'`).toBeDefined();
		expect(promptBlock!).toMatch(/role="(?:status|alert)"/);
		expect(promptBlock!).toMatch(/email/i);
		expect(promptBlock!).not.toContain('Sign-in failed');
	});
});

describe('login passkey genuine failure (unchanged)', () => {
	it('still lands on the exact generic error copy after the ceremony starts', () => {
		const source = readLoginPage();

		// The error assignment survives, but only in the post-start
		// (ceremony/catch) part of the handler.
		const start = source.indexOf('async function signInWithPasskey()');
		const afterWorking = source.slice(
			start + source.slice(start).search(/passkeyStatus\s*=\s*'working'/)
		);
		expect(afterWorking).toMatch(/passkeyStatus\s*=\s*'error'/);

		const errorBlock = source.match(
			/\{(?:#if|:else if) passkeyStatus === 'error'\}([\s\S]*?)(?=\{:else|\{\/if\})/
		)?.[1];
		expect(errorBlock, "expected a rendered branch for passkeyStatus === 'error'").toBeDefined();
		expect(errorBlock!).toMatch(/role="alert"/);
		expect(errorBlock!).toContain(LOGIN_GENERIC);
	});

	it('keeps the dismissed-ceremony copy untouched', () => {
		const source = readLoginPage();

		const dismissedBlock = source.match(
			/\{(?:#if|:else if) passkeyStatus === 'dismissed'\}([\s\S]*?)(?=\{:else|\{\/if\})/
		)?.[1];
		expect(
			dismissedBlock,
			"expected a rendered branch for passkeyStatus === 'dismissed'"
		).toBeDefined();
		expect(dismissedBlock!).toContain('You dismissed the passkey prompt');
	});
});
