import { KNOWN_ERROR_CODES } from '../api/errors.js';

/**
 * Plan-limit (402/429) handling: turn a limit response into a paywall model
 * instead of a dead-end error toast.
 *
 * Server-side limit codes are owned by the API (see `e2e/helpers/contract.ts`
 * `PLAN_LIMIT_STATUSES`); this module consumes them, it does not redefine
 * them:
 *
 * - 402 -> terminal: the plan limit is exhausted for the period. The only
 *   way forward is an upgrade, so the paywall shows current usage and an
 *   upgrade CTA to `/pricing`.
 * - 429 -> retryable: the same paywall, plus a retry countdown parsed from
 *   the `Retry-After` header (delta-seconds or HTTP-date). The user can wait
 *   out the countdown or upgrade immediately.
 *
 * Analytics: when the paywall is displayed, `trackPaywallShown` fires the
 * event {@link PAYWALL_SHOWN_EVENT} (`usage_limit_paywall_shown`) exactly
 * once with coarse limit facts only — `{ kind, status, code,
 * retryAfterSeconds }`. Never PII: no email, user id, token, or password.
 * The default sink in the app layout re-dispatches it as a same-named
 * `CustomEvent` on `window` for whatever analytics host is attached.
 *
 * Display plumbing: API clients call {@link paywallFromResponse} on any
 * non-OK response; it builds the model (null for non-limit statuses) and
 * notifies listeners registered via {@link onPaywall}. The root layout is
 * the listener and renders `PaywallModal`. In the browser, a response received
 * before the root layout subscribes is retained and replayed to the first
 * listener so cold-load failures cannot disappear; SSR calls remain a no-op.
 */

/** Documented analytics event fired once per paywall display. No PII. */
export const PAYWALL_SHOWN_EVENT = 'usage_limit_paywall_shown';

/** The upgrade CTA always lands on pricing; the page owns plan selection. */
export const PRICING_HREF = '/pricing';

export interface PaywallModel {
	/** `terminal` (402: upgrade is the only path) or `retryable` (429: wait or upgrade). */
	kind: 'terminal' | 'retryable';
	/** HTTP status that produced this model (402 or 429). */
	status: number;
	/** Server limit code (`plan_limit_exceeded`, `rate_limited`, ...) when the body named one. */
	code: string | null;
	ctaHref: '/pricing';
	headline: string;
	/** Names the current usage (exact figures when the body carried them). */
	detail: string;
	/** Seconds until retry may succeed; null when unknown or not applicable. */
	retryAfterSeconds: number | null;
	/** Human countdown line for `retryAfterSeconds`; null when there is nothing to count. */
	countdownText: string | null;
	/** Explicit UI retry only; omitted when dismissal should just restore focus. Never analytics. */
	retry?: () => void;
}

export interface LimitResponseLike {
	status: number;
	headers: Headers;
	/** Parsed JSON body when the caller already read it; tolerated in any shape. */
	body?: unknown;
}

export type TrackFn = (name: string, props: Record<string, unknown>) => void;

function asRecord(value: unknown): Record<string, unknown> | null {
	return typeof value === 'object' && value !== null ? (value as Record<string, unknown>) : null;
}

/** First finite number among `keys` on `record` (or its `usage`/`data` nest). */
function pickNumber(record: Record<string, unknown>, keys: string[]): number | null {
	for (const key of keys) {
		const value = record[key];
		if (typeof value === 'number' && Number.isFinite(value)) return value;
	}
	for (const nest of ['usage', 'data']) {
		const inner = asRecord(record[nest]);
		if (inner) {
			const found = pickNumber(inner, keys);
			if (found !== null) return found;
		}
	}
	return null;
}

function pickString(record: Record<string, unknown>, keys: string[]): string | null {
	for (const key of keys) {
		const value = record[key];
		if (typeof value === 'string' && value.length > 0) return value;
	}
	return null;
}

/** Server limit code from `{ code }`, `{ error: "code" }`, or `{ error: { code } }`. */
function isKnownErrorCode(code: string): boolean {
	return KNOWN_ERROR_CODES.has(code) || code === 'indexer_auth_expired';
}

function bodyCode(body: unknown): string | null {
	const record = asRecord(body);
	if (!record) return null;
	const direct = pickString(record, ['code']);
	if (direct && isKnownErrorCode(direct)) return direct;
	// A string `error` only counts as a code when it is registered; a prose
	// message or unknown legacy value is not an analytics code.
	if (typeof record.error === 'string' && isKnownErrorCode(record.error)) {
		return record.error;
	}
	const nested = pickString(asRecord(record.error) ?? {}, ['code']);
	return nested && isKnownErrorCode(nested) ? nested : null;
}

/**
 * "Current usage: X of Y on the Free plan." when the body carried figures
 * (key names mirror `extractPlanMeta` in the e2e contract); otherwise a
 * generic line that still names the usage rather than a bare error.
 */
function usageDetail(body: unknown, terminal: boolean): string {
	const record = asRecord(body);
	if (record) {
		const used = pickNumber(record, ['used_bytes', 'usage_bytes', 'used']);
		const limit = pickNumber(record, ['limit_bytes', 'plan_limit_bytes', 'limit']);
		const tier = pickString(record, ['plan', 'plan_tier', 'tier']);
		if (used !== null && limit !== null) {
			const plan = tier ? ` on the ${tier} plan` : ' on your current plan';
			return (
				`Current usage: ${used.toLocaleString()} of ${limit.toLocaleString()}${plan}. ` +
				(terminal
					? 'Upgrade to keep going now, or wait for the next period.'
					: 'Wait for the countdown, or upgrade to keep going now.')
			);
		}
	}
	return terminal
		? "Your current usage has reached your plan's limit. Upgrade to keep going now, or wait for the next period."
		: "Your current usage is at your plan's limit for now. Wait for the countdown, or upgrade to keep going now.";
}

/**
 * Upper bound (one hour) for any `Retry-After` wait. The server should never
 * send a longer wait, but a stale cache, a misbehaving proxy, or a far-future
 * HTTP-date could; without a cap the paywall countdown would render a
 * non-finite wait (`Infinity minutes NaN seconds`) and its per-second timer
 * would never reach try-again-now. Every parsed wait above this clamps to it.
 */
export const MAX_RETRY_AFTER_SECONDS = 3600;

/**
 * Parse `Retry-After` (delta-seconds or HTTP-date) into whole seconds from
 * `nowMs`. Returns null when the header is absent or unparseable; a date in
 * the past clamps to 0 (retry immediately). Waits above
 * {@link MAX_RETRY_AFTER_SECONDS} clamp to it so the countdown text stays
 * finite and the per-second timer always terminates.
 */
export function parseRetryAfter(headers: Headers, nowMs: number = Date.now()): number | null {
	const raw = headers.get('Retry-After') ?? headers.get('retry-after');
	if (raw === null) return null;
	const trimmed = raw.trim();
	if (/^\d+$/.test(trimmed)) return Math.min(Number.parseInt(trimmed, 10), MAX_RETRY_AFTER_SECONDS);
	const at = Date.parse(trimmed);
	if (Number.isNaN(at)) return null;
	return Math.min(MAX_RETRY_AFTER_SECONDS, Math.max(0, Math.ceil((at - nowMs) / 1000)));
}

/** `45` -> "Try again in 45 seconds."; `125` -> minutes + seconds; `0` -> now. */
export function formatCountdown(totalSeconds: number): string {
	const seconds = Math.max(0, Math.floor(totalSeconds));
	if (seconds === 0) return 'You can try again now.';
	if (seconds < 60) return `Try again in ${seconds} second${seconds === 1 ? '' : 's'}.`;
	const minutes = Math.floor(seconds / 60);
	const rest = seconds % 60;
	const minutePart = `${minutes} minute${minutes === 1 ? '' : 's'}`;
	return rest === 0
		? `Try again in ${minutePart}.`
		: `Try again in ${minutePart} ${rest} second${rest === 1 ? '' : 's'}.`;
}

/**
 * Build the paywall model for a 402/429 limit response, or null for any
 * other status (those keep their existing error handling). A limit status
 * always yields a model — never null, never a dead-end toast.
 */
export function toPaywallModel(res: LimitResponseLike): PaywallModel | null {
	if (res.status !== 402 && res.status !== 429) return null;
	const terminal = res.status === 402;
	const retryAfterSeconds = terminal ? null : parseRetryAfter(res.headers);
	return {
		kind: terminal ? 'terminal' : 'retryable',
		status: res.status,
		code: bodyCode(res.body),
		ctaHref: PRICING_HREF,
		headline: terminal ? "You've reached your plan limit" : 'Please slow down a little',
		detail: usageDetail(res.body, terminal),
		retryAfterSeconds,
		countdownText: retryAfterSeconds === null ? null : formatCountdown(retryAfterSeconds)
	};
}

/**
 * Fire {@link PAYWALL_SHOWN_EVENT} exactly once for a displayed paywall.
 * Props are coarse limit facts only (`kind`, `status`, `code`,
 * `retryAfterSeconds`) — never PII.
 */
export function trackPaywallShown(track: TrackFn, model: PaywallModel): void {
	track(PAYWALL_SHOWN_EVENT, {
		kind: model.kind,
		status: model.status,
		code: model.code,
		retryAfterSeconds: model.retryAfterSeconds
	});
}

type PaywallListener = (model: PaywallModel) => void;

const listeners = new Set<PaywallListener>();
let pendingModel: PaywallModel | null = null;

/** Subscribe to paywall displays (root layout). Returns the unsubscribe. */
export function onPaywall(listener: PaywallListener): () => void {
	listeners.add(listener);
	if (pendingModel !== null) {
		const pending = pendingModel;
		pendingModel = null;
		notifyListener(listener, pending);
	}
	return () => {
		listeners.delete(listener);
	};
}

function notifyListener(listener: PaywallListener, model: PaywallModel): void {
	try {
		listener(model);
	} catch {
		/* display is best-effort; the API error still propagates */
	}
}

/** Notify display listeners, retaining the latest model until one subscribes. */
export function showPaywall(model: PaywallModel): void {
	if (listeners.size === 0) {
		if (typeof window !== 'undefined') pendingModel = model;
		return;
	}
	for (const listener of listeners) notifyListener(listener, model);
}

/**
 * Client entry point: build the model for a response and, when it is a
 * limit response, notify the display listener. Returns the model (or null)
 * so callers can also attach it to their typed error.
 */
export function paywallFromResponse(res: LimitResponseLike, retry?: () => void): PaywallModel | null {
	const model = toPaywallModel(res);
	if (model !== null) {
		if (model.kind === 'retryable' && retry) model.retry = retry;
		showPaywall(model);
	}
	return model;
}
