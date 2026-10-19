/**
 * Typed client for the billing/usage REST surface `apps/web` consumes.
 *
 * Expected backend contract (sibling backend cards implement; this file is
 * the frontend half of that handshake):
 *
 * - GET /v1/plans -> { plans: PlanDTO[] }
 * - GET /v1/me/usage -> { period, streamSecondsMonth, concurrentNow, libraryTracks, ingestToday }
 * - GET /v1/me/subscription -> { planId, status, currentPeriodEnd, trialEndsAt }
 * - POST /v1/billing/checkout { planId } -> { url } (Stripe Checkout, 303-style body)
 * - POST /v1/billing/portal {} -> { url } (Stripe customer portal)
 *
 * Auth: session cookie (credentials: include). Entitlement checks run
 * server-side on every stream start; these endpoints are display + redirect
 * helpers only. No Stripe secret ever touches this code.
 */

import { paywallFromResponse, type PaywallModel } from './planLimit.js';
import { isJsonBodyTooLarge, readCappedJson } from '../api/cappedJson.js';
import { parseErrorBody } from '../api/errors.js';
import type { UsageSnapshot } from './usage';

export interface PlanDTO {
	id: string;
	priceCents: number;
	streamHoursMonth: number;
	maxConcurrentStreams: number;
	maxLibraryTracks: number;
	ingestJobsDay: number;
	cachePriority: string;
}

export interface SubscriptionDTO {
	planId: string;
	/**
	 * Stripe statuses stored verbatim (ADR 0002 data-model, ADR 0005
	 * billing table): `incomplete`, `incomplete_expired`, `trialing`,
	 * `active`, `past_due`, `canceled`, `unpaid`, `paused`. There is no
	 * local `expired` shortcut. Only `active`/`trialing` (plus `past_due`
	 * inside its grace window) grant entitlement — every other stored
	 * status routes to the billing portal via the loader's LAPSED set.
	 */
	status:
		| 'trialing'
		| 'active'
		| 'past_due'
		| 'canceled'
		| 'unpaid'
		| 'paused'
		| 'incomplete'
		| 'incomplete_expired';
	currentPeriodEnd: string | null;
	trialEndsAt: string | null;
	/** End of the past-due grace window (ADR 0002 rule 4); absent when the backend sends none. */
	graceUntil?: string | null;
}

/**
 * Typed fetch failure carrying the HTTP status so loaders can tell a signed-out
 * visitor (401) or a missing record (404) apart from a backend outage. The
 * message keeps the status code so existing `toThrow('404')` assertions hold.
 */
export class BillingApiError extends Error {
	readonly status: number;
	/** Paywall model for 402/429 failures; null for other billing errors. */
	readonly paywall: PaywallModel | null;
	/** Registry code from the error envelope (`docs/api-errors.md`). */
	readonly code: string;
	/** Whether retrying the same request may succeed (envelope flag). */
	readonly retryable: boolean;

	constructor(
		status: number,
		label: string,
		hint = '',
		code = 'request_failed',
		retryable = false,
		paywall: PaywallModel | null = null
	) {
		super(`${label} failed (${status})${hint}`);
		this.name = 'BillingApiError';
		this.status = status;
		this.code = code;
		this.retryable = retryable;
		this.paywall = paywall;
	}
}

/**
 * Successful GET /v1/me/usage DTO guard. `period` follows the existing
 * `YYYY-MM` contract (month 01–12); usage fields are required finite
 * nonnegative numbers. Unknown extra fields are allowed; valid zero usage
 * is preserved. Anything else rejects into the existing recoverable loader
 * path instead of reaching usage evaluation/rendering.
 */
const USAGE_PERIOD_RE = /^\d{4}-(0[1-9]|1[0-2])$/;

const USAGE_NUMERIC_FIELDS = [
	'streamSecondsMonth',
	'concurrentNow',
	'libraryTracks',
	'ingestToday'
] as const;

function isFiniteNonNegativeNumber(value: unknown): value is number {
	return typeof value === 'number' && Number.isFinite(value) && value >= 0;
}

function isValidUsagePayload(data: unknown): data is UsageSnapshot & { period: string } {
	if (typeof data !== 'object' || data === null || Array.isArray(data)) return false;
	const record = data as Record<string, unknown>;
	if (typeof record.period !== 'string' || !USAGE_PERIOD_RE.test(record.period)) return false;
	for (const field of USAGE_NUMERIC_FIELDS) {
		if (!isFiniteNonNegativeNumber(record[field])) return false;
	}
	return true;
}

/**
 * Successful GET /v1/me/subscription DTO guard. `planId` is a required
 * non-empty string; `status` must be one of the eight Stripe statuses the
 * subscriptions row stores verbatim (ADR 0002 data-model, ADR 0005
 * billing table) so a raw unknown status never reaches rendering — and
 * subscribers in a stored non-entitled state (`unpaid`, `paused`,
 * `incomplete_expired`) keep their portal route instead of failing into
 * trial defaults. `currentPeriodEnd` and `trialEndsAt` are required and
 * are either null or an ISO-8601 date string naming a real calendar date
 * — garbage strings and rolled-over dates (e.g. February 30, which
 * `Date.parse` normalizes into March) reject instead of rendering
 * "Invalid Date" or a shifted renewal day. `graceUntil` may be absent;
 * when present it is null or a valid date string. Unknown extra fields
 * are allowed. Anything else rejects into the existing recoverable
 * loader path instead of reaching rendering.
 */
const SUBSCRIPTION_STATUSES = [
	'trialing',
	'active',
	'past_due',
	'canceled',
	'unpaid',
	'paused',
	'incomplete',
	'incomplete_expired'
] as const;

const SUBSCRIPTION_DATE_FIELDS = ['currentPeriodEnd', 'trialEndsAt'] as const;

/**
 * ISO-8601 date/datetime with bounded fields (month 01–12, day 01–31,
 * hour 00–23), so loose strings `Date.parse` accepts (`'1'`, `'garbage'`)
 * never pass. A UTC round-trip on the calendar date rejects impossible
 * dates the parser would roll over (`2026-02-30` -> March 2).
 */
const SUBSCRIPTION_DATE_RE =
	/^(\d{4})-(0[1-9]|1[0-2])-(0[1-9]|[12]\d|3[01])(?:T([01]\d|2[0-3]):[0-5]\d(?::[0-5]\d(?:\.\d{1,9})?)?(?:Z|[+-](?:[01]\d|2[0-3]):?[0-5]\d))?$/;

function isParseableDateString(value: unknown): value is string {
	if (typeof value !== 'string' || value.length === 0) return false;
	const match = SUBSCRIPTION_DATE_RE.exec(value);
	if (!match) return false;
	const year = Number(match[1]);
	const month = Number(match[2]);
	const day = Number(match[3]);
	// Date.UTC(year, ...) treats 00–99 as 1900–1999; preserve the literal ISO year.
	const probe = new Date(0);
	probe.setUTCFullYear(year, month - 1, day);
	if (
		probe.getUTCFullYear() !== year ||
		probe.getUTCMonth() !== month - 1 ||
		probe.getUTCDate() !== day
	)
		return false;
	return !Number.isNaN(Date.parse(value));
}

function isValidSubscriptionPayload(data: unknown): data is SubscriptionDTO {
	if (typeof data !== 'object' || data === null || Array.isArray(data)) return false;
	const record = data as Record<string, unknown>;
	if (typeof record.planId !== 'string' || record.planId.length === 0) return false;
	if (
		typeof record.status !== 'string' ||
		!(SUBSCRIPTION_STATUSES as readonly string[]).includes(record.status)
	)
		return false;
	for (const field of SUBSCRIPTION_DATE_FIELDS) {
		const value = record[field];
		if (value !== null && !isParseableDateString(value)) return false;
	}
	if ('graceUntil' in record) {
		const graceUntil = record.graceUntil;
		if (graceUntil !== undefined && graceUntil !== null && !isParseableDateString(graceUntil))
			return false;
	}
	return true;
}

/**
 * Successful GET /v1/plans DTO guard. The body must be an object whose
 * `plans` field is an array (possibly empty). Unknown extra envelope
 * fields are allowed and entries pass through verbatim; per-plan field
 * validation is out of scope. Anything else rejects into the existing
 * recoverable loader path instead of reaching pricing rendering as
 * `undefined`, a string, or any other non-array value.
 */
function isValidPlansPayload(data: unknown): data is { plans: PlanDTO[] } {
	if (typeof data !== 'object' || data === null || Array.isArray(data)) return false;
	return Array.isArray((data as Record<string, unknown>).plans);
}

async function readJson<T>(res: Response, label: string): Promise<T> {
	if (!res.ok) {
		let body: unknown = null;
		try {
			body = await readCappedJson(res);
		} catch (error) {
			// An over-cap error body is never parsed: the generic status
			// fallback applies, never retryable, with no paywall model
			// emitted.
			if (isJsonBodyTooLarge(error)) {
				const parsed = parseErrorBody(null, res.status);
				const hint = parsed.message ? `: ${parsed.message}` : '';
				throw new BillingApiError(res.status, label, hint, parsed.code, false, null);
			}
			/* non-JSON error body: use status-based defaults */
		}
		const parsed = parseErrorBody(body, res.status);
		// Client-side calls surface 402/429 through the layout listener; if it
		// has not registered yet, the pending model replays on subscription.
		// During SSR, no model is queued and the typed error remains the fallback.
		const paywall = paywallFromResponse({ status: res.status, headers: res.headers, body });
		const hint = parsed.message ? `: ${parsed.message}` : '';
		throw new BillingApiError(res.status, label, hint, parsed.code, parsed.retryable, paywall);
	}
	try {
		return (await readCappedJson(res)) as T;
	} catch (error) {
		// An over-cap 2xx never parses: surface the typed error, never a bare
		// SyntaxError, never retryable, never a paywall, and never leaking
		// body bytes (generic status message).
		if (isJsonBodyTooLarge(error)) {
			const parsed = parseErrorBody(null, res.status);
			const hint = parsed.message ? `: ${parsed.message}` : '';
			throw new BillingApiError(res.status, label, hint, parsed.code, false, null);
		}
		// A 2xx whose body is not JSON is a backend contract violation, not
		// a usable result: surface the typed error keeping the caller's
		// label — never a bare SyntaxError, and never a downstream TypeError
		// from reading fields off an undecodable body. Same generic fallback
		// as undecodable error bodies, but never retryable and never a paywall.
		// Only decode failures are rewrapped: transport errors rethrow
		// untouched so they stay distinguishable.
		if (!(error instanceof SyntaxError)) throw error;
		const parsed = parseErrorBody(null, res.status);
		const hint = parsed.message ? `: ${parsed.message}` : '';
		throw new BillingApiError(res.status, label, hint, parsed.code, false, null);
	}
}

export async function fetchPlans(fetchFn: typeof fetch = fetch): Promise<PlanDTO[]> {
	const res = await fetchFn('/v1/plans', { credentials: 'include' });
	const data = await readJson<unknown>(res, 'Load plans');
	if (!isValidPlansPayload(data)) throw new Error('Invalid plans payload');
	return data.plans;
}

export async function fetchUsage(
	fetchFn: typeof fetch = fetch
): Promise<UsageSnapshot & { period: string }> {
	const res = await fetchFn('/v1/me/usage', { credentials: 'include' });
	const data = await readJson<unknown>(res, 'Load usage');
	if (!isValidUsagePayload(data)) throw new Error('Invalid usage payload');
	return data;
}

export async function fetchSubscription(fetchFn: typeof fetch = fetch): Promise<SubscriptionDTO> {
	const res = await fetchFn('/v1/me/subscription', { credentials: 'include' });
	const data = await readJson<unknown>(res, 'Load subscription');
	if (!isValidSubscriptionPayload(data)) throw new Error('Invalid subscription payload');
	return data;
}

/**
 * Authorized Stripe redirect hosts for checkout/portal 2xx bodies. Any
 * allowlisted `https:` target resolves verbatim; everything else rejects
 * with the typed client error so an off-allowlist or malformed target never
 * reaches a `window.location.href` assignment (open-redirect guard).
 */
const STRIPE_REDIRECT_HOSTS = new Set(['checkout.stripe.com', 'billing.stripe.com']);

function isAllowlistedRedirectUrl(value: unknown): value is string {
	if (typeof value !== 'string' || value.trim() === '') return false;
	// WHATWG URL parsing recovers an allowlisted host from a scheme without
	// an explicit authority (`https:checkout.stripe.com/...` parses with
	// hostname checkout.stripe.com), but a `window.location.href` assignment
	// of that same string resolves against the app origin instead — so the
	// raw string must carry the explicit `https://` authority up front.
	if (!value.startsWith('https://')) return false;
	let parsed: URL;
	try {
		parsed = new URL(value);
	} catch {
		return false;
	}
	return parsed.protocol === 'https:' && STRIPE_REDIRECT_HOSTS.has(parsed.hostname.toLowerCase());
}

function redirectUrlOrThrow(
	data: unknown,
	status: number,
	label: string,
	hint: string
): string {
	if (typeof data === 'object' && data !== null && !Array.isArray(data)) {
		const url = (data as Record<string, unknown>).url;
		if (isAllowlistedRedirectUrl(url)) return url;
	}
	throw new BillingApiError(status, label, hint);
}

export async function startCheckout(
	planId: string,
	fetchFn: typeof fetch = fetch
): Promise<string> {
	if (!planId || /[^a-z0-9-]/.test(planId)) throw new Error('Invalid plan');
	const res = await fetchFn('/v1/billing/checkout', {
		method: 'POST',
		credentials: 'include',
		headers: { 'content-type': 'application/json' },
		body: JSON.stringify({ planId })
	});
	const data = await readJson<unknown>(res, 'Start checkout');
	return redirectUrlOrThrow(data, res.status, 'Start checkout', ': Invalid checkout URL');
}

/**
 * Signup URL for a signed-out visitor whose checkout attempt failed with
 * 401. Returns null for any other error — those stay as inline alerts.
 * The chosen plan rides along so /signup can continue into checkout.
 */
export function signupUrlForCheckoutFailure(planId: string, err: unknown): string | null {
	if (err instanceof BillingApiError && err.status === 401) {
		return `/signup?plan=${encodeURIComponent(planId)}`;
	}
	return null;
}

export async function openPortal(fetchFn: typeof fetch = fetch): Promise<string> {
	const res = await fetchFn('/v1/billing/portal', {
		method: 'POST',
		credentials: 'include',
		headers: { 'content-type': 'application/json' },
		body: '{}'
	});
	const data = await readJson<unknown>(res, 'Open billing portal');
	return redirectUrlOrThrow(data, res.status, 'Open billing portal', ': Invalid portal URL');
}
