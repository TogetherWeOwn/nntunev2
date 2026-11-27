/**
 * Typed client for the billing/usage REST surface `apps/web` consumes.
 *
 * Expected backend contract (sibling backend cards implement; this file is
 * the frontend half of that handshake):
 *
 * - GET /v1/plans -> { plans: PlanDTO[] }
 * - GET /v1/me/usage -> { period, streamSecondsMonth, resetsAt }
 * - GET /v1/me/subscription -> { state, planId, status, currentPeriodEnd, graceUntil, cancelAtPeriodEnd }
 * - POST /v1/billing/checkout { planId } -> { url } (Stripe Checkout, 303-style body)
 * - POST /v1/billing/portal {} -> { url } (Stripe customer portal)
 *
 * Auth: session cookie (credentials: include). Entitlement checks run
 * server-side on every stream start; these endpoints are display + redirect
 * helpers only. No Stripe secret ever touches this code.
 */

import { paywallFromResponse, type PaywallModel } from './planLimit.js';
import type { AccountSession } from '../auth-fence.js';
import { isJsonBodyTooLarge, readCappedJson } from '../api/cappedJson.js';
import { parseErrorBody } from '../api/errors.js';
import { isValidPlanRow, parseBillingDate, type PlanDTO } from './plans';
import { isNonNegativeFiniteNumber, isUsagePeriod, type UsageSnapshot } from './usage';

export type { PlanDTO } from './plans';

/** Server-derived account states (spec §4); a missing row is a state, not an error. */
export type SubscriptionState = 'no_subscription' | 'active' | 'lapsed';

export interface SubscriptionDTO {
	/**
	 * Server-derived state (ADR 0005 resolution — settlement, past-due
	 * grace, unknown-status fail-closed — folded into the closed cases).
	 * Authoritative for display; the client never re-derives entitlement.
	 */
	state: SubscriptionState;
	/** The live row's plan, or null when there is no live row. */
	planId: string | null;
	/**
	 * Stripe statuses stored verbatim (ADR 0002 data-model, ADR 0005
	 * billing table): `incomplete`, `incomplete_expired`, `trialing`,
	 * `active`, `past_due`, `canceled`, `unpaid`, `paused`. There is no
	 * local `expired` shortcut and no trial product state — a stored
	 * `trialing` is unknown to the current model and folds into `lapsed`.
	 * Null when there is no live row. Stored non-entitled statuses route
	 * to the billing portal via the loader's LAPSED set.
	 */
	status:
		| 'trialing'
		| 'active'
		| 'past_due'
		| 'canceled'
		| 'unpaid'
		| 'paused'
		| 'incomplete'
		| 'incomplete_expired'
		| null;
	currentPeriodEnd: string | null;
	/** End of the past-due grace window (ADR 0002 rule 4). */
	graceUntil: string | null;
	/** True when cancellation is scheduled at the current period end. */
	cancelAtPeriodEnd: boolean;
	/** False in the first-purchase pilot, where existing plans use the portal. */
	canChangePlan?: boolean;
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
 * `YYYY-MM` contract (month 01–12); `streamSecondsMonth` is a required
 * finite nonnegative number; `resetsAt` must be the start of the next UTC
 * month (spec §3.2). Unknown extra
 * fields are allowed; valid zero usage is preserved. Anything else
 * rejects into the existing recoverable loader path instead of reaching
 * usage evaluation/rendering.
 */
function isValidUsagePayload(
	data: unknown
): data is UsageSnapshot & { period: string; resetsAt: string } {
	if (typeof data !== 'object' || data === null || Array.isArray(data)) return false;
	const record = data as Record<string, unknown>;
	if (!isUsagePeriod(record.period) || !isNonNegativeFiniteNumber(record.streamSecondsMonth)) {
		return false;
	}
	const nextMonth = new Date(0);
	nextMonth.setUTCFullYear(Number(record.period.slice(0, 4)), Number(record.period.slice(5)), 1);
	return parseBillingDate(record.resetsAt) === nextMonth.getTime();
}

/**
 * Successful GET /v1/me/subscription DTO guard. The endpoint answers 200
 * for every session, so `state` is required and must be one of the three
 * server-derived cases; `planId`/`status` are either null (no live row)
 * or, respectively, a non-empty string and one of the eight Stripe
 * statuses the subscriptions row stores verbatim (ADR 0002 data-model,
 * ADR 0005 billing table), so a raw unknown status never reaches
 * rendering — and subscribers in a stored non-entitled state (`unpaid`,
 * `paused`, `incomplete_expired`) keep their portal route instead of
 * failing into defaults. `currentPeriodEnd` is required and is either
 * null or an ISO-8601 date string naming a real calendar date — garbage
 * strings and rolled-over dates (e.g. February 30, which `Date.parse`
 * normalizes into March) reject instead of rendering "Invalid Date" or
 * a shifted renewal day. `graceUntil` may be absent; when present it is
 * null or a valid date string. `cancelAtPeriodEnd` must be a boolean.
 * Unknown extra fields are allowed. Anything else rejects into the
 * existing recoverable loader path instead of reaching rendering.
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

const SUBSCRIPTION_STATES = ['no_subscription', 'active', 'lapsed'] as const;

function isValidSubscriptionPayload(data: unknown): data is SubscriptionDTO {
	if (typeof data !== 'object' || data === null || Array.isArray(data)) return false;
	const record = data as Record<string, unknown>;
	if (
		typeof record.state !== 'string' ||
		!(SUBSCRIPTION_STATES as readonly string[]).includes(record.state)
	)
		return false;
	if (
		record.planId !== null &&
		(typeof record.planId !== 'string' || record.planId.length === 0)
	)
		return false;
	if (
		record.status !== null &&
		(typeof record.status !== 'string' ||
			!(SUBSCRIPTION_STATUSES as readonly string[]).includes(record.status))
	)
		return false;
	if (record.currentPeriodEnd !== null && parseBillingDate(record.currentPeriodEnd) === null)
		return false;
	if ('graceUntil' in record) {
		const graceUntil = record.graceUntil;
		if (graceUntil !== undefined && graceUntil !== null && parseBillingDate(graceUntil) === null)
			return false;
	}
	if (typeof record.cancelAtPeriodEnd !== 'boolean') return false;
	return record.canChangePlan === undefined || typeof record.canChangePlan === 'boolean';
}

/**
 * Successful GET /v1/plans DTO guard. The body must be an object whose
 * `plans` field is an array (possibly empty) of valid rows with unique IDs.
 * Unknown extra fields (envelope or row) are allowed.
 * Anything else — including one malformed row among good ones — rejects
 * the whole payload into the existing recoverable loader path, so a
 * partial row can never hydrate over the pricing fallback and render
 * `$NaN/mo` in place of every plan.
 */
function isValidPlansPayload(data: unknown): data is { plans: PlanDTO[] } {
	if (typeof data !== 'object' || data === null || Array.isArray(data)) return false;
	const plans = (data as Record<string, unknown>).plans;
	if (!Array.isArray(plans)) return false;
	const ids = new Set<string>();
	for (const row of plans) {
		if (!isValidPlanRow(row) || ids.has(row.id)) return false;
		ids.add(row.id);
	}
	return true;
}

/** One budget for dispatch, headers and JSON consumption, including error bodies. */
const BILLING_REQUEST_TIMEOUT_MS = 30_000;

async function requestJson<T>(
	path: string,
	label: string,
	fetchFn: typeof fetch,
	init: RequestInit = {},
	owner?: AccountSession
): Promise<{ data: T; status: number }> {
	const controller = new AbortController();
	let timedOut = false;
	const timeoutError = () => new BillingApiError(
		504,
		label,
		`: request timed out after ${BILLING_REQUEST_TIMEOUT_MS}ms deadline`,
		'request_timeout',
		true
	);
	let rejectDeadline!: (error: Error) => void;
	const deadline = new Promise<never>((_resolve, reject) => { rejectDeadline = reject; });
	const timer = setTimeout(() => {
		timedOut = true;
		const error = timeoutError();
		rejectDeadline(error);
		controller.abort(error);
	}, BILLING_REQUEST_TIMEOUT_MS);
	const stale = () => new DOMException('Account session ended', 'AbortError');
	const assertCurrent = () => {
		if (owner && !owner.isCurrent()) throw stale();
	};
	const onRetire = () => {
		const error = stale();
		rejectDeadline(error);
		controller.abort(error);
	};
	owner?.signal.addEventListener('abort', onRetire, { once: true });

	try {
		assertCurrent();
		// Race as well as abort: an injected transport may ignore the signal.
		const headers = fetchFn(path, {
			...init,
			...(owner && { cache: 'no-store', redirect: 'error' } as const),
			credentials: 'include',
			signal: controller.signal
		}).then((res) => {
			if (timedOut || (owner && !owner.isCurrent())) {
				void res.body?.cancel().catch(() => {});
				throw timedOut ? timeoutError() : stale();
			}
			return res;
		});
		const res = await Promise.race([headers, deadline]);
		assertCurrent();
		if (!res.ok) {
			let body: unknown = null;
			try {
				// Time bounds the read; the byte cap bounds how much is
				// buffered before parsing. Both apply to error bodies.
				body = await Promise.race([readCappedJson(res), deadline]);
			} catch (error) {
				assertCurrent();
				// Expiry is an outage, not a signed-out or paywall response.
				if (timedOut) throw timeoutError();
				// An over-cap error body is never parsed: the generic status
				// fallback applies, never retryable, with no paywall model
				// emitted.
				if (isJsonBodyTooLarge(error)) {
					const parsed = parseErrorBody(null, res.status);
					const hint = parsed.message ? `: ${parsed.message}` : '';
					throw new BillingApiError(res.status, label, hint, parsed.code, false, null);
				}
				// A complete non-JSON response can use status defaults. An
				// interrupted read must propagate as a transport failure.
				if (!(error instanceof SyntaxError)) throw error;
			}
			assertCurrent();
			const parsed = parseErrorBody(body, res.status);
			// Client-side calls surface 402/429 through the layout listener; if it
			// has not registered yet, the pending model replays on subscription.
			// During SSR, no model is queued and the typed error remains the fallback.
			const paywall = paywallFromResponse({ status: res.status, headers: res.headers, body });
			const hint = parsed.message ? `: ${parsed.message}` : '';
			throw new BillingApiError(res.status, label, hint, parsed.code, parsed.retryable, paywall);
		}
		try {
			// Time bounds the read; the byte cap bounds how much is buffered.
			const data = (await Promise.race([readCappedJson(res), deadline])) as T;
			assertCurrent();
			return { data, status: res.status };
		} catch (error) {
			assertCurrent();
			if (timedOut) throw timeoutError();
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
	} finally {
		clearTimeout(timer);
		owner?.signal.removeEventListener('abort', onRetire);
	}
}

export async function fetchPlans(fetchFn: typeof fetch = fetch): Promise<PlanDTO[]> {
	const { data } = await requestJson<unknown>('/v1/plans', 'Load plans', fetchFn);
	if (!isValidPlansPayload(data)) throw new Error('Invalid plans payload');
	return data.plans;
}

export async function fetchUsage(
	fetchFn: typeof fetch = fetch,
	owner?: AccountSession
): Promise<UsageSnapshot & { period: string; resetsAt: string }> {
	const { data } = await requestJson<unknown>('/v1/me/usage', 'Load usage', fetchFn, {}, owner);
	if (!isValidUsagePayload(data)) throw new Error('Invalid usage payload');
	return data;
}

export async function fetchSubscription(fetchFn: typeof fetch = fetch, owner?: AccountSession): Promise<SubscriptionDTO> {
	const { data } = await requestJson<unknown>('/v1/me/subscription', 'Load subscription', fetchFn, {}, owner);
	if (!isValidSubscriptionPayload(data)) throw new Error('Invalid subscription payload');
	return data;
}

/**
 * Stripe destinations are pinned to the origin for the billing action.
 * A valid target resolves verbatim; everything else rejects
 * with the typed client error so an off-allowlist or malformed target never
 * reaches a `window.location.href` assignment (open-redirect guard).
 */
function isAllowedRedirectUrl(value: unknown, origin: string): value is string {
	if (typeof value !== 'string') return false;
	// WHATWG URL parsing recovers an allowlisted host from a scheme without
	// an explicit authority (`https:checkout.stripe.com/...` parses with
	// hostname checkout.stripe.com), but a `window.location.href` assignment
	// of that same string resolves against the app origin instead — so the
	// raw string must carry the explicit `https://` authority up front.
	if (!/^https:\/\/[^/\\?#@\s]+(?=[/?#]|$)/.test(value)) return false;
	let parsed: URL;
	try {
		parsed = new URL(value);
	} catch {
		return false;
	}
	return parsed.origin === origin;
}

function redirectUrlOrThrow(
	data: unknown,
	status: number,
	label: string,
	hint: string,
	origin: string
): string {
	if (typeof data === 'object' && data !== null && !Array.isArray(data)) {
		const url = (data as Record<string, unknown>).url;
		if (isAllowedRedirectUrl(url, origin)) return url;
	}
	throw new BillingApiError(status, label, hint);
}

export async function startCheckout(
	planId: string,
	fetchFn: typeof fetch = fetch,
	owner?: AccountSession
): Promise<string> {
	if (!planId || /[^a-z0-9-]/.test(planId)) throw new Error('Invalid plan');
	const { data, status } = await requestJson<unknown>('/v1/billing/checkout', 'Start checkout', fetchFn, {
		method: 'POST',
		redirect: 'error',
		headers: { 'content-type': 'application/json' },
		body: JSON.stringify({ planId })
	}, owner);
	return redirectUrlOrThrow(data, status, 'Start checkout', ': Invalid checkout URL', 'https://checkout.stripe.com');
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

export async function openPortal(fetchFn: typeof fetch = fetch, owner?: AccountSession): Promise<string> {
	const { data, status } = await requestJson<unknown>('/v1/billing/portal', 'Open billing portal', fetchFn, {
		method: 'POST',
		redirect: 'error',
		headers: { 'content-type': 'application/json' },
		body: '{}'
	}, owner);
	return redirectUrlOrThrow(data, status, 'Open billing portal', ': Invalid portal URL', 'https://billing.stripe.com');
}
