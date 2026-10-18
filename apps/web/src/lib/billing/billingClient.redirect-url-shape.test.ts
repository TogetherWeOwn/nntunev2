/**
 * Acceptance spec (protected — builders must not edit this file).
 *
 * Contract for `startCheckout` / `openPortal` successful-response handling:
 *
 * - A 2xx body is accepted only when it is an object whose `url` is a string
 *   holding an absolute `https:` URL on the authorized redirect host set
 *   (Stripe Checkout / Stripe Billing Portal hosts, as already used by the
 *   valid cases in `billingClient.test.ts`).
 * - Every other 2xx shape — missing/non-string `url`, empty/blank `url`,
 *   non-https scheme, off-allowlist https host, host-lookalike or
 *   userinfo-smuggling URL — rejects with the typed `BillingApiError`
 *   (never a bare `TypeError`, never a resolved string a caller could assign
 *   to `window.location.href`). The typed error preserves the real 2xx
 *   status, uses the default `request_failed` code, is not retryable, carries
 *   no paywall model, and its message names the checkout/portal action
 *   without leaking the offending body or URL.
 * - Valid allowlisted https URLs still resolve verbatim (query strings and
 *   unknown extra response fields are tolerated).
 *
 * Non-JSON 2xx bodies and non-2xx handling are covered elsewhere and are out
 * of scope here. Injected fetch only; no network.
 */
import { describe, expect, it, vi } from 'vitest';
import { BillingApiError, openPortal, startCheckout } from './billingClient';

function ok(body: unknown): Response {
	return new Response(JSON.stringify(body), { status: 200 });
}

function fetchReturning(body: unknown): typeof fetch {
	return vi.fn(async () => ok(body)) as unknown as typeof fetch;
}

/**
 * Asserts the full typed-shape contract for one rejected redirect attempt.
 * `actionPattern` pins which action the message must name; `forbidden`
 * substrings (offending hosts, engine error text) must never leak into it.
 */
async function expectTypedShapeError(
	attempt: Promise<string>,
	actionPattern: RegExp,
	forbidden: string[] = []
): Promise<void> {
	const failure: unknown = await attempt.catch((e: unknown) => e);
	expect(failure, 'redirect attempt must reject, not resolve').toBeInstanceOf(Error);
	expect(failure).not.toBeInstanceOf(TypeError);
	expect(failure).toBeInstanceOf(BillingApiError);
	const err = failure as BillingApiError;
	expect(err.name).toBe('BillingApiError');
	expect(err.status).toBe(200);
	expect(err.code).toBe('request_failed');
	expect(err.retryable).toBe(false);
	expect(err.paywall).toBeNull();
	expect(err.message).toMatch(actionPattern);
	expect(err.message).not.toMatch(/cannot read properties/i);
	expect(err.message).not.toMatch(/startsWith is not a function/i);
	for (const leak of forbidden) {
		expect(err.message).not.toContain(leak);
	}
}

/** 2xx bodies that parse as JSON but carry no usable string `url`. */
const MALFORMED_BODIES: Array<[string, unknown]> = [
	['missing url ({})', {}],
	['null url', { url: null }],
	['numeric url', { url: 12345 }],
	['boolean url', { url: true }],
	['object url', { url: { href: 'https://checkout.stripe.com/c/pay_1' } }],
	['empty array url', { url: [] }],
	['array url wrapping a valid target', { url: ['https://checkout.stripe.com/c/pay_1'] }],
	['empty string url', { url: '' }],
	['blank string url', { url: '   ' }],
	['top-level null body', null],
	['top-level string body', 'https://checkout.stripe.com/c/pay_1'],
	['top-level array body', [{ url: 'https://checkout.stripe.com/c/pay_1' }]],
	['top-level number body', 42]
];

const CHECKOUT_OFF_ALLOWLIST: Array<[string, string]> = [
	['unrelated https host', 'https://evil.example/phish'],
	['host-suffix lookalike', 'https://checkout.stripe.com.evil.example/c/pay_1'],
	['userinfo-smuggled host', 'https://checkout.stripe.com@evil.example/c/pay_1'],
	['IP-literal host', 'https://203.0.113.7/c/pay_1']
];

const PORTAL_OFF_ALLOWLIST: Array<[string, string]> = [
	['unrelated https host', 'https://evil.example/phish'],
	['host-suffix lookalike', 'https://billing.stripe.com.evil.example/p/session_1'],
	['userinfo-smuggled host', 'https://billing.stripe.com@evil.example/p/session_1'],
	['IP-literal host', 'https://203.0.113.7/p/session_1']
];

/** Right host family or well-formed string, wrong scheme. */
const CHECKOUT_BAD_SCHEMES: Array<[string, string]> = [
	['plain http on the allowlisted host', 'http://checkout.stripe.com/c/pay_1'],
	['javascript scheme', 'javascript:alert(1)'],
	['data scheme', 'data:text/html;base64,PHNjcmlwdD5hbGVydCgxKTwvc2NyaXB0Pg==']
];

const PORTAL_BAD_SCHEMES: Array<[string, string]> = [
	['plain http on the allowlisted host', 'http://billing.stripe.com/p/session_1'],
	['javascript scheme', 'javascript:alert(1)'],
	['data scheme', 'data:text/html;base64,PHNjcmlwdD5hbGVydCgxKTwvc2NyaXB0Pg==']
];

describe('startCheckout malformed 2xx bodies reject as typed errors', () => {
	it.each(MALFORMED_BODIES)('%s', async (_name, body) => {
		await expectTypedShapeError(startCheckout('plus', fetchReturning(body)), /checkout/i);
	});
});

describe('openPortal malformed 2xx bodies reject as typed errors', () => {
	it.each(MALFORMED_BODIES)('%s', async (_name, body) => {
		await expectTypedShapeError(openPortal(fetchReturning(body)), /portal/i);
	});
});

describe('startCheckout off-allowlist https targets reject as typed errors', () => {
	it.each(CHECKOUT_OFF_ALLOWLIST)('%s', async (_name, url) => {
		await expectTypedShapeError(startCheckout('plus', fetchReturning({ url })), /checkout/i, [
			'evil.example',
			'203.0.113.7'
		]);
	});
});

describe('openPortal off-allowlist https targets reject as typed errors', () => {
	it.each(PORTAL_OFF_ALLOWLIST)('%s', async (_name, url) => {
		await expectTypedShapeError(openPortal(fetchReturning({ url })), /portal/i, [
			'evil.example',
			'203.0.113.7'
		]);
	});
});

describe('non-https redirect targets reject as typed errors', () => {
	it.each(CHECKOUT_BAD_SCHEMES)('checkout: %s', async (_name, url) => {
		await expectTypedShapeError(startCheckout('plus', fetchReturning({ url })), /checkout/i);
	});

	it.each(PORTAL_BAD_SCHEMES)('portal: %s', async (_name, url) => {
		await expectTypedShapeError(openPortal(fetchReturning({ url })), /portal/i);
	});
});

describe('valid allowlisted https URLs still resolve verbatim', () => {
	it('resolves the checkout URL', async () => {
		await expect(
			startCheckout('plus', fetchReturning({ url: 'https://checkout.stripe.com/c/pay_123' }))
		).resolves.toBe('https://checkout.stripe.com/c/pay_123');
	});

	it('resolves the portal URL', async () => {
		await expect(
			openPortal(fetchReturning({ url: 'https://billing.stripe.com/p/session_1' }))
		).resolves.toBe('https://billing.stripe.com/p/session_1');
	});

	it('preserves query strings on valid targets', async () => {
		const url = 'https://checkout.stripe.com/c/pay_123?prefilled_email=a%40b.example';
		await expect(startCheckout('plus', fetchReturning({ url }))).resolves.toBe(url);
	});

	it('tolerates unknown extra fields alongside a valid url', async () => {
		await expect(
			openPortal(
				fetchReturning({ url: 'https://billing.stripe.com/p/session_1', sessionId: 'bps_1', live: false })
			)
		).resolves.toBe('https://billing.stripe.com/p/session_1');
	});
});

/**
 * Callers do `window.location.href = await startCheckout(...)` /
 * `= await openPortal()` (pricing and account/billing pages). Rejection is
 * the guarantee an off-allowlist or malformed target never reaches the
 * assignment; these cases pin that caller-visible outcome.
 */
describe('caller redirect assignment', () => {
	it('never assigns an off-allowlist checkout URL to location.href', async () => {
		const location = { href: 'https://app.example/pricing' };
		const assign = (async () => {
			location.href = await startCheckout(
				'plus',
				fetchReturning({ url: 'https://evil.example/phish' })
			);
		})();
		await expectTypedShapeError(assign.then(() => location.href), /checkout/i, ['evil.example']);
		expect(location.href).toBe('https://app.example/pricing');
	});

	it('never assigns a malformed portal response to location.href', async () => {
		const location = { href: 'https://app.example/account/billing' };
		const assign = (async () => {
			location.href = await openPortal(fetchReturning({}));
		})();
		await expectTypedShapeError(assign.then(() => location.href), /portal/i);
		expect(location.href).toBe('https://app.example/account/billing');
	});

	it('assigns a valid allowlisted checkout URL to location.href', async () => {
		const location = { href: 'https://app.example/pricing' };
		location.href = await startCheckout(
			'plus',
			fetchReturning({ url: 'https://checkout.stripe.com/c/pay_123' })
		);
		expect(location.href).toBe('https://checkout.stripe.com/c/pay_123');
	});
});
