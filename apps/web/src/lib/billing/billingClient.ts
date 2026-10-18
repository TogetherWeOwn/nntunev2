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
	 * ADR 0002 statuses (`trialing`/`active`/`past_due`/`canceled`/`expired`),
	 * plus Stripe's `incomplete` tolerated while payment confirms — never
	 * entitlement-granting either way.
	 */
	status: 'trialing' | 'active' | 'past_due' | 'canceled' | 'expired' | 'incomplete';
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

/** One budget for dispatch, headers and JSON consumption, including error bodies. */
const BILLING_REQUEST_TIMEOUT_MS = 30_000;

async function requestJson<T>(
	path: string,
	label: string,
	fetchFn: typeof fetch,
	init: RequestInit = {}
): Promise<T> {
	const controller = new AbortController();
	let timedOut = false;
	const timeoutError = () => new BillingApiError(
		504,
		label,
		`: request timed out after ${BILLING_REQUEST_TIMEOUT_MS}ms deadline`,
		'request_timeout',
		true
	);
	let rejectDeadline!: (error: BillingApiError) => void;
	const deadline = new Promise<never>((_resolve, reject) => { rejectDeadline = reject; });
	const timer = setTimeout(() => {
		timedOut = true;
		const error = timeoutError();
		rejectDeadline(error);
		controller.abort(error);
	}, BILLING_REQUEST_TIMEOUT_MS);

	try {
		// Race as well as abort: injected transports may ignore the signal.
		// Their late responses must never reach JSON/paywall classification.
		const headers = fetchFn(path, {
			...init,
			credentials: 'include',
			signal: controller.signal
		}).then((res) => {
			if (timedOut) {
				// Best-effort disposal only: cancellation may reject or never settle.
				void res.body?.cancel().catch(() => {});
				throw timeoutError();
			}
			return res;
		});
		const res = await Promise.race([headers, deadline]);
		if (!res.ok) {
			let body: unknown = null;
			try {
				body = await Promise.race([res.json(), deadline]);
			} catch {
				// Expiry is an outage, not a signed-out or paywall response.
				if (timedOut) throw timeoutError();
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
			return (await Promise.race([res.json(), deadline])) as T;
		} catch (error) {
			if (timedOut) throw timeoutError();
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
	}
}

export async function fetchPlans(fetchFn: typeof fetch = fetch): Promise<PlanDTO[]> {
	const data = await requestJson<unknown>('/v1/plans', 'Load plans', fetchFn);
	if (!isValidPlansPayload(data)) throw new Error('Invalid plans payload');
	return data.plans;
}

export async function fetchUsage(
	fetchFn: typeof fetch = fetch
): Promise<UsageSnapshot & { period: string }> {
	const data = await requestJson<unknown>('/v1/me/usage', 'Load usage', fetchFn);
	if (!isValidUsagePayload(data)) throw new Error('Invalid usage payload');
	return data;
}

export async function fetchSubscription(fetchFn: typeof fetch = fetch): Promise<SubscriptionDTO> {
	return await requestJson('/v1/me/subscription', 'Load subscription', fetchFn);
}

export async function startCheckout(
	planId: string,
	fetchFn: typeof fetch = fetch
): Promise<string> {
	if (!planId || /[^a-z0-9-]/.test(planId)) throw new Error('Invalid plan');
	const data = await requestJson<{ url: string }>('/v1/billing/checkout', 'Start checkout', fetchFn, {
		method: 'POST',
		headers: { 'content-type': 'application/json' },
		body: JSON.stringify({ planId })
	});
	if (!data.url.startsWith('https://')) throw new Error('Invalid checkout URL');
	return data.url;
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
	const data = await requestJson<{ url: string }>('/v1/billing/portal', 'Open billing portal', fetchFn, {
		method: 'POST',
		headers: { 'content-type': 'application/json' },
		body: '{}'
	});
	if (!data.url.startsWith('https://')) throw new Error('Invalid portal URL');
	return data.url;
}
