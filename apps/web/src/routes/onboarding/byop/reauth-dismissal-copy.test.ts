import { describe, expect, it } from 'vitest';

/**
 * Builder-owned pin: the BYOP reauth dismissed notice must hedge the cause.
 *
 * `NotAllowedError` / `AbortError` can mean dismissal, timeout, or an
 * unavailable passkey (see `isPasskeyDismissal` on this page and the
 * login/signup siblings), so the dismissed copy enumerates the possible
 * reasons instead of asserting the user dismissed the prompt. Reverting to
 * the asserting copy ("You dismissed the passkey prompt — try again when
 * ready.") must turn this spec red, while the shared substring keeps the
 * acceptance suites green.
 *
 * Source-pinned via the same raw-glob approach as `page.test.ts` in this
 * directory (no `node:` imports, so `svelte-check` stays green).
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

describe('byop reauth dismissal copy hedges the cause', () => {
	it('enumerates possible reasons instead of asserting dismissal', () => {
		const source = readByopPage();

		const dismissedBlock = source.match(
			/reauthState === 'dismissed'\}([\s\S]*?)(?:\{:else|\{\/if\})/
		)?.[1];
		expect(dismissedBlock, "expected a {#if reauthState === 'dismissed'} block").toBeDefined();
		const block = dismissedBlock!;

		// Shared substring with the acceptance suites (stays green).
		expect(block).toMatch(/dismissed the passkey prompt/i);
		// The hedge: reverting to the asserting copy drops these and turns
		// this spec red.
		expect(block).toMatch(/Possible reasons/i);
		expect(block).toMatch(/timed out/i);
		expect(block).toMatch(/no usable passkey was available/i);
		// Never the failure banner or its sign-in-again dead end.
		expect(block).not.toMatch(/Reauthentication failed/);
		expect(block).not.toMatch(/Sign in again/);
	});
});
