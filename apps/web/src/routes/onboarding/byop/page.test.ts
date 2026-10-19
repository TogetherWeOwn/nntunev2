import { describe, expect, it } from 'vitest';
import { render } from 'svelte/server';
import ByopPage from './+page.svelte';

/**
 * Acceptance: BYOP passkey reauthentication must not report failure for
 * a paywall response or a dismissed passkey prompt.
 *
 * `reauthenticateAndSave` funnels every rejection into
 * `reauthState = 'failed'` ("Reauthentication failed. Sign in again to
 * continue."). Two rejections are not failures, matching the
 * paywall-aware catches in `runTest`/`save` on this page and the
 * dismissal handling in the login/signup pages:
 *
 * - `ApiError` with a paywall model (402/429 from reauthenticate
 *   start/finish): the global paywall modal owns the response, so the
 *   reauth state returns to `idle` — no failure banner beneath the modal.
 * - A `NotAllowedError`/`AbortError` ceremony rejection: the user
 *   dismissed the platform prompt, so the state is `dismissed` and the
 *   rendered copy says so, without the sign-in-again dead end.
 *
 * Genuine failures keep landing on `failed` with the exact current
 * banner. SSR `render` only produces the initial state, so the
 * interactive branches are pinned against the route source (same
 * raw-glob approach as `src/routes/signup/page.test.ts`). The
 * behavioral twin is `tests/byop-reauth-paywall-dismissal.spec.ts`.
 */
const sources = import.meta.glob('./+page.svelte', {
	query: '?raw',
	import: 'default',
	eager: true
}) as Record<string, string>;

function readByopPage(): string {
	const source = Object.values(sources)[0];
	if (typeof source !== 'string' || source.length === 0) {
		throw new Error(
			'missing byop page source: src/routes/onboarding/byop/+page.svelte was not found by the raw glob'
		);
	}
	return source;
}

/** The `reauthenticateAndSave` function body. */
function reauthBody(source: string): string {
	const fn = source.match(/async function reauthenticateAndSave\(\) \{([\s\S]*?)\n\t\}/);
	expect(fn, 'expected a reauthenticateAndSave() function').not.toBeNull();
	return fn![1];
}

describe('byop page initial state', () => {
	it('renders step 1 with no reauth failure or dismissal copy', () => {
		const { body } = render(ByopPage);

		expect(body).toMatch(/<h1[^>]*>Connect your provider<\/h1>/);
		expect(body).toContain('data-testid="byop-host"');
		expect(body).not.toContain('Reauthentication failed');
		expect(body).not.toMatch(/dismissed the passkey prompt/i);
	});
});

describe('byop reauth paywall and dismissal states', () => {
	it('paywalled reauth returns to the idle (paywall) state with no failure banner', () => {
		const body = reauthBody(readByopPage());

		const catchAt = body.indexOf('catch');
		expect(catchAt, 'expected a catch around the reauth ceremony').toBeGreaterThanOrEqual(0);
		const inCatch = body.slice(catchAt);
		// The paywall branch mirrors runTest/save on this page: an ApiError
		// carrying a paywall model resets the reauth state to idle and
		// returns, before the failure assignment is reached.
		expect(inCatch).toMatch(
			/error instanceof ApiError && error\.paywall\s*\)\s*\{[\s\S]*?reauthState\s*=\s*'idle';[\s\S]*?return;/
		);
		const paywallAt = inCatch.search(/error\.paywall/);
		const failedAt = inCatch.search(/reauthState\s*=\s*'failed'/);
		expect(paywallAt).toBeGreaterThanOrEqual(0);
		expect(failedAt).toBeGreaterThan(paywallAt);
	});

	it('dismissed passkey prompt yields the dismissed state, not failure', () => {
		const source = readByopPage();
		const body = reauthBody(source);

		// The reauth state machine gains a dismissed state...
		expect(source).toMatch(/reauthState\s*=\s*\$state<[^>]*'dismissed'[^>]*>/);
		// ...reached only for the two ceremony-dismissal error names (the
		// predicate may be a helper, as in login/signup, or inline).
		expect(source).toMatch(/'NotAllowedError'/);
		expect(source).toMatch(/'AbortError'/);
		const inCatch = body.slice(body.indexOf('catch'));
		const dismissedAt = inCatch.search(/reauthState\s*=\s*'dismissed'/);
		const failedAt = inCatch.search(/reauthState\s*=\s*'failed'/);
		expect(dismissedAt, 'expected the catch to set the dismissed state').toBeGreaterThanOrEqual(0);
		expect(failedAt).toBeGreaterThan(dismissedAt);
		// The dismissed branch renders dismissal copy — never the failure
		// banner or its sign-in-again dead end.
		const dismissedBlock = source.match(
			/reauthState === 'dismissed'\}([\s\S]*?)(?:\{:else|\{\/if\})/
		)?.[1];
		expect(dismissedBlock, "expected a {#if reauthState === 'dismissed'} block").toBeDefined();
		const block = dismissedBlock!;
		expect(block).toMatch(/dismissed the passkey prompt/i);
		expect(block).not.toMatch(/Reauthentication failed/);
		expect(block).not.toMatch(/Sign in again/);
	});

	it('genuine reauth failures keep the exact current failure banner', () => {
		const source = readByopPage();
		const body = reauthBody(source);

		// The catch still falls through to the failed state for every
		// other rejection, and a paywall/dismissal must not fall into the
		// save retry below the catch.
		const inCatch = body.slice(body.indexOf('catch'));
		expect(inCatch).toMatch(/reauthState\s*=\s*'failed';\s*return;/);
		// The failed branch keeps its exact current copy and exit link,
		// scoped so moving them out of the branch turns the spec red.
		const failedBlock = source.match(
			/reauthState === 'failed'\}([\s\S]*?)(?:\{:else|\{\/if\})/
		)?.[1];
		expect(failedBlock, "expected a {#if reauthState === 'failed'} block").toBeDefined();
		const block = failedBlock!;
		expect(block).toContain('Reauthentication failed.');
		expect(block).toMatch(/<a href="\/login">Sign in again<\/a>/);
		expect(block).not.toMatch(/dismissed the passkey prompt/i);
	});
});
