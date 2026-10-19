/**
 * Typed fetch client for `workers/api`. The web app talks only to the API
 * worker — never directly to NNTP providers, Hyperdrive, R2 or KV.
 *
 * The request/response shapes here mirror the API worker's OpenAPI spec
 * (published by the backend cards); once codegen lands, the hand-written
 * types below are replaced by the generated client without changing call
 * sites, because all requests flow through {@link ApiClient}.
 */

import { paywallFromResponse, type PaywallModel } from '../billing/planLimit.js';
import type { ProviderForm } from '../vault-validation.js';
import { isJsonBodyTooLarge, readCappedJson } from './cappedJson.js';
import { parseErrorBody } from './errors.js';

export type FetchFn = typeof fetch;

export interface ApiClientOptions {
	baseUrl: string;
	fetchFn?: FetchFn;
}

export class ApiError extends Error {
	readonly status: number;
	/** Registry code from the error envelope (`docs/api-errors.md`). */
	readonly code: string;
	/**
	 * Paywall model for 402/429 plan-limit responses (already shown via the
	 * layout listener); null for every other failure. Callers that also
	 * render an inline error can use this to avoid duplicating the paywall.
	 */
	readonly paywall: PaywallModel | null;
	/** Whether retrying the same request may succeed (envelope flag). */
	readonly retryable: boolean;

	constructor(
		status: number,
		code: string,
		message: string,
		paywall: PaywallModel | null = null,
		retryable = false
	) {
		super(message);
		this.name = 'ApiError';
		this.status = status;
		this.code = code;
		this.paywall = paywall;
		this.retryable = retryable;
	}
}

/** Documented re-auth code (S7): stale provider credential. */
export const PROVIDER_AUTH_EXPIRED_CODE = 'provider_auth_expired';

/** Documented re-key code (S7): stale indexer API key. Distinct from the
 * provider code: the web app names the API key (never the password) for it. */
export const INDEXER_AUTH_EXPIRED_CODE = 'indexer_auth_expired';

/**
 * Deadline for a single ApiClient request (headers + body). Hung auth/vault
 * calls must abort instead of wedging login/signup/BYOP spinners forever.
 * Well under the acceptance cap (10 fake minutes) and above the
 * still-pending probe (1s); fake-timer driven via `setTimeout` so vitest
 * controls it (unlike `AbortSignal.timeout`'s internal timer).
 */
export const API_REQUEST_TIMEOUT_MS = 30_000;

export interface MagicLinkResult {
	ok: true;
	/**
	 * Present on magic-link finish only: the signed-in account's own
	 * email, echoed back so the web app can show "Signed in as …" before
	 * any provider credential is typed (login-CSRF defense in depth).
	 */
	email?: string;
}

export interface IndexerConfigInput {
	url: string;
	api_key: string;
}

/** Wire shape used by workers/api's serde ProviderConfig and IndexerConfig. */
export interface ProviderTestInput {
	host: string;
	port: number;
	tls: boolean;
	allow_insecure_plaintext: boolean;
	username: string;
	password: string;
	max_connections: number;
	indexer?: IndexerConfigInput;
}

/**
 * Save/test envelope the API Worker expects (`PUT /v1/vault`,
 * `POST /v1/vault/test`): the flat provider fields nested under
 * `provider`, exactly as `workers/api` parses `VaultPutBody`. Field names
 * are pinned by `workers/api/tests/fixtures/api-web-contract.json`, which
 * both the vitest client suite and the Rust handlers suite consume — change
 * both sides in the same PR, never one alone.
 */
export interface VaultSaveBody {
	provider: {
		host: string;
		port: number;
		tls: boolean;
		allow_insecure_plaintext: boolean;
		username: string;
		password: string;
		max_connections: number;
	};
	indexer?: IndexerConfigInput;
}

export function toVaultSaveBody(input: ProviderTestInput): VaultSaveBody {
	const { indexer, ...provider } = input;
	return indexer === undefined ? { provider } : { provider, indexer };
}

export function toProviderTestInput(
	provider: ProviderForm,
	indexerUrl: string,
	indexerApiKey: string
): ProviderTestInput {
	return {
		host: provider.host.trim(),
		port: provider.port,
		tls: provider.tls,
		// Only send the opt-in with plaintext: with TLS it is always false so
		// the server never records a spurious acknowledgement.
		allow_insecure_plaintext: !provider.tls && provider.allowInsecurePlaintext,
		username: provider.username.trim(),
		password: provider.password,
		max_connections: provider.maxConnections,
		...(indexerUrl.trim().length > 0
			? { indexer: { url: indexerUrl.trim(), api_key: indexerApiKey.trim() } }
			: {})
	};
}

export interface ProviderTestResult {
	ok: boolean;
	latencyMs?: number;
	error?: string;
}

/** Redacted vault metadata only; never return credentials or endpoint details. */
export interface SavedProviderSummary {
	provider_configured: boolean;
	indexer_configured: boolean;
	/** Unix seconds, or zero when no configuration is stored. */
	updated_at: number;
}

function validateSavedProviderSummary(data: unknown, status: number): SavedProviderSummary {
	if (typeof data !== 'object' || data === null || Array.isArray(data)) {
		throw vaultTestShapeFailure(status);
	}
	const record = data as Record<string, unknown>;
	if (
		typeof record.provider_configured !== 'boolean' ||
		typeof record.indexer_configured !== 'boolean' ||
		!isFiniteNonNegativeNumber(record.updated_at)
	) {
		throw vaultTestShapeFailure(status);
	}
	return {
		provider_configured: record.provider_configured,
		indexer_configured: record.indexer_configured,
		updated_at: record.updated_at
	};
}

function isFiniteNonNegativeNumber(value: unknown): value is number {
	return typeof value === 'number' && Number.isFinite(value) && value >= 0;
}

/** Typed shape failure for a malformed vault-test success body. */
function vaultTestShapeFailure(status: number): ApiError {
	const parsed = parseErrorBody(null, status);
	return new ApiError(status, 'request_failed', parsed.message, null, false);
}

/**
 * Guard the `POST /v1/vault/test` success body before any caller can show
 * "Connected": the body must be a plain object whose `ok` is a real boolean
 * (so `{ok:"false"}` can never unlock Save via truthiness), while an invalid
 * `latencyMs` is display-only and therefore omitted, never rejected. A string
 * `error` passes through for logging; anything else is dropped.
 */
function validateVaultTestResult(data: unknown, status: number): ProviderTestResult {
	if (typeof data !== 'object' || data === null || Array.isArray(data)) {
		throw vaultTestShapeFailure(status);
	}
	const record = data as Record<string, unknown>;
	if (typeof record.ok !== 'boolean') throw vaultTestShapeFailure(status);
	const result: ProviderTestResult = { ok: record.ok };
	if (record.latencyMs !== undefined && isFiniteNonNegativeNumber(record.latencyMs)) {
		result.latencyMs = record.latencyMs;
	}
	if (typeof record.error === 'string') result.error = record.error;
	return result;
}

/** `POST /v1/auth/passkey/register/start` response (fixed shape, never an oracle). */
export interface RegisterStartResponse {
	challenge: string;
	user_id: string;
	rp_id: string;
}

/** `POST /v1/auth/passkey/login/start` response (empty allow-list by design). */
export interface LoginStartResponse {
	challenge: string;
	allow_credentials: unknown[];
	rp_id: string;
}

/** Typed shape failure for a malformed passkey-start success body. */
function passkeyStartShapeFailure(status: number): ApiError {
	const parsed = parseErrorBody(null, status);
	return new ApiError(status, 'request_failed', parsed.message, null, false);
}

function isNonEmptyString(value: unknown): value is string {
	return typeof value === 'string' && value.length > 0;
}

/**
 * Guard the register-start success body before any ceremony can begin: the
 * body must be a plain object whose `challenge`, `user_id` and `rp_id` are
 * each a non-empty string. A malformed `challenge` would otherwise throw
 * inside base64url decoding mid-ceremony as a generic ceremony error instead
 * of a typed contract failure.
 */
function validateRegisterStart(data: unknown, status: number): RegisterStartResponse {
	if (typeof data !== 'object' || data === null || Array.isArray(data)) {
		throw passkeyStartShapeFailure(status);
	}
	const record = data as Record<string, unknown>;
	if (
		!isNonEmptyString(record.challenge) ||
		!isNonEmptyString(record.user_id) ||
		!isNonEmptyString(record.rp_id)
	) {
		throw passkeyStartShapeFailure(status);
	}
	return {
		challenge: record.challenge,
		user_id: record.user_id,
		rp_id: record.rp_id
	};
}

/**
 * Guard the login-start (and session-reauth-start) success body the same way:
 * a plain object whose `challenge` and `rp_id` are each a non-empty string.
 * The server-provided `allow_credentials` list passes through untouched.
 */
function validateLoginStart(data: unknown, status: number): LoginStartResponse {
	if (typeof data !== 'object' || data === null || Array.isArray(data)) {
		throw passkeyStartShapeFailure(status);
	}
	const record = data as Record<string, unknown>;
	if (!isNonEmptyString(record.challenge) || !isNonEmptyString(record.rp_id)) {
		throw passkeyStartShapeFailure(status);
	}
	return {
		challenge: record.challenge,
		allow_credentials: Array.isArray(record.allow_credentials)
			? record.allow_credentials
			: [],
		rp_id: record.rp_id
	};
}

/**
 * Inputs for the passkey finish calls. Pages pass camelCase fields and the
 * builders below emit the snake_case wire body the Worker parses
 * (`deny_unknown_fields`), so a renamed field fails type-checking in the
 * page and the fixture test in the client — never silently in production.
 * Wire shapes are pinned by `api-web-contract.json`.
 */
export interface RegisterFinishInput {
	challenge: string;
	credentialId: string;
	attestationObject: string;
	clientDataJson: string;
	magicToken: string;
	inviteCode?: string;
}

export interface LoginFinishInput {
	challenge: string;
	credentialId: string;
	authenticatorData: string;
	clientDataJson: string;
	signature: string;
}

export function toRegisterFinishBody(input: RegisterFinishInput): Record<string, string> {
	return {
		challenge: input.challenge,
		credential_id: input.credentialId,
		attestation_object: input.attestationObject,
		client_data_json: input.clientDataJson,
		magic_token: input.magicToken,
		...(input.inviteCode !== undefined && input.inviteCode.length > 0
			? { invite_code: input.inviteCode }
			: {})
	};
}

export function toLoginFinishBody(input: LoginFinishInput): Record<string, string> {
	return {
		challenge: input.challenge,
		credential_id: input.credentialId,
		authenticator_data: input.authenticatorData,
		client_data_json: input.clientDataJson,
		signature: input.signature
	};
}

export class ApiClient {
	private readonly baseUrl: string;
	private readonly fetchFn: FetchFn;
	private token: string | null = null;

	constructor({ baseUrl, fetchFn }: ApiClientOptions) {
		this.baseUrl = baseUrl.replace(/\/+$/, '');
		// Default to a wrapper (not a bare `fetch` reference): calling a stored
		// `fetch` as `this.fetchFn(...)` sets `this` to the client instance and
		// browsers/workerd reject it with "Illegal invocation".
		this.fetchFn = fetchFn ?? ((input, init) => fetch(input, init));
	}

	setToken(token: string | null): void {
		this.token = token;
	}

	async get<T>(path: string): Promise<T> {
		return this.request<T>(path, { method: 'GET' });
	}

	async post<T>(path: string, body: unknown): Promise<T> {
		return this.request<T>(path, {
			method: 'POST',
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify(body)
		});
	}

	async put<T>(path: string, body: unknown): Promise<T> {
		return this.request<T>(path, {
			method: 'PUT',
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify(body)
		});
	}

	/**
	 * Request an email magic link. Always resolves the same way for unknown
	 * and known addresses (the API never reveals which emails exist).
	 */
	async requestMagicLink(email: string): Promise<MagicLinkResult> {
		return this.post<MagicLinkResult>('/v1/auth/magic/start', { email });
	}

	/** Redeem an emailed magic-link token (the `/auth/magic` page). */
	async finishMagicLink(token: string): Promise<MagicLinkResult> {
		return this.post<MagicLinkResult>('/v1/auth/magic/finish', { token });
	}

	/**
	 * Sign out of the current session. The server revokes the session and
	 * clears the HttpOnly cookie; the caller also drops its display-only
	 * identity label (see `signed-in-email.ts`). Used by the `/auth/magic`
	 * error state so a session-swapped user can sign out and redeem the
	 * other account's link — the server's magic-finish rejection stays a
	 * generic 401 either way.
	 */
	async logout(): Promise<void> {
		await this.post<unknown>('/v1/session/logout', {});
	}

	/** Revoke every session for the authenticated account. */
	async revokeAllSessions(): Promise<{ ok: true }> {
		return this.post<{ ok: true }>('/v1/session/revoke-all', {});
	}

	/**
	 * Start passkey enrollment; the API also mails the inbox-proof code. The
	 * success body is shape-checked before it returns so a malformed
	 * `challenge` surfaces as a typed `ApiError`, never a mid-ceremony throw.
	 */
	async registerStart(email: string): Promise<RegisterStartResponse> {
		const { data, status } = await this.requestWithStatus<unknown>(
			'/v1/auth/passkey/register/start',
			{
				method: 'POST',
				headers: { 'content-type': 'application/json' },
				body: JSON.stringify({ email })
			}
		);
		return validateRegisterStart(data, status);
	}

	/** Finish passkey enrollment with the attestation + emailed code. */
	async registerFinish(input: RegisterFinishInput): Promise<MagicLinkResult> {
		return this.post<MagicLinkResult>(
			'/v1/auth/passkey/register/finish',
			toRegisterFinishBody(input)
		);
	}

	/**
	 * Start passkey login for an email (empty allow-list response). The
	 * success body is shape-checked before it returns, as in `registerStart`.
	 */
	async loginStart(email: string): Promise<LoginStartResponse> {
		const { data, status } = await this.requestWithStatus<unknown>(
			'/v1/auth/passkey/login/start',
			{
				method: 'POST',
				headers: { 'content-type': 'application/json' },
				body: JSON.stringify({ email })
			}
		);
		return validateLoginStart(data, status);
	}

	/** Finish passkey login with the assertion. */
	async loginFinish(input: LoginFinishInput): Promise<MagicLinkResult> {
		return this.post<MagicLinkResult>(
			'/v1/auth/passkey/login/finish',
			toLoginFinishBody(input)
		);
	}

	/**
	 * Start a passkey proof bound to the currently authenticated session. The
	 * success body carries the login-start shape and is checked the same way.
	 */
	async reauthenticateStart(signal?: AbortSignal): Promise<LoginStartResponse> {
		const { data, status } = await this.requestWithStatus<unknown>(
			'/v1/session/reauthenticate/start',
			{
				signal,
				method: 'POST',
				headers: { 'content-type': 'application/json' },
				body: JSON.stringify({})
			}
		);
		return validateLoginStart(data, status);
	}

	/** Complete proof-bound reauthentication without affecting ordinary login. */
	async reauthenticateFinish(input: LoginFinishInput, signal?: AbortSignal): Promise<MagicLinkResult> {
		return this.request<MagicLinkResult>('/v1/session/reauthenticate/finish', {
			method: 'POST',
			signal,
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify(toLoginFinishBody(input))
		});
	}

	/** Read a validated, allowlisted summary of the saved vault. */
	async getSavedProviderSummary(signal?: AbortSignal): Promise<SavedProviderSummary> {
		const { data, status } = await this.requestWithStatus<unknown>('/v1/vault', {
			method: 'GET', signal
		});
		return validateSavedProviderSummary(data, status);
	}

	/** Reachability only, not provider authentication. An empty body selects the stored vault. */
	async testSavedProviderConnection(signal?: AbortSignal): Promise<ProviderTestResult> {
		const { data, status } = await this.requestWithStatus<unknown>('/v1/vault/test', {
			method: 'POST', signal
		});
		return validateVaultTestResult(data, status);
	}

	/** No automatic retries: only a validated both-false summary proves removal. */
	async deleteSavedProviderConfig(signal?: AbortSignal): Promise<SavedProviderSummary> {
		const { data, status } = await this.requestWithStatus<unknown>('/v1/vault', {
			method: 'DELETE', signal
		});
		const summary = validateSavedProviderSummary(data, status);
		if (summary.provider_configured || summary.indexer_configured) {
			throw vaultTestShapeFailure(status);
		}
		return summary;
	}

	/**
	 * Live-test a provider (+ optional indexer) config without storing it.
	 * The API validates (SSRF guard), dials, and returns a redacted result —
	 * secrets never come back in the response.
	 */
	async testProviderConnection(input: ProviderTestInput): Promise<ProviderTestResult> {
		const { data, status } = await this.requestWithStatus<unknown>('/v1/vault/test', {
			method: 'POST',
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify(toVaultSaveBody(input))
		});
		return validateVaultTestResult(data, status);
	}

	/**
	 * Store a tested provider (+ optional indexer) config in the encrypted
	 * vault. The API seals secrets with envelope encryption; responses are
	 * redacted summaries, never secret material.
	 */
	async saveProviderConfig(input: ProviderTestInput): Promise<MagicLinkResult> {
		return this.put<MagicLinkResult>('/v1/vault', toVaultSaveBody(input));
	}

	private async request<T>(path: string, init: RequestInit): Promise<T> {
		return (await this.requestWithStatus<T>(path, init)).data;
	}

	private async requestWithStatus<T>(
		path: string,
		init: RequestInit
	): Promise<{ data: T; status: number }> {
		const headers: Record<string, string> = {
			...((init.headers as Record<string, string> | undefined) ?? {}),
			accept: 'application/json'
		};
		if (this.token !== null) headers['authorization'] = `Bearer ${this.token}`;

		// Per-request deadline: abort hung fetches AND stalled body reads so
		// auth/vault spinners always settle. The timer is cleared on settle so
		// fast requests never wait for it and later requests are unaffected.
		const controller = new AbortController();
		const callerSignal = init.signal as AbortSignal | null | undefined;
		const onCallerAbort = (): void => controller.abort(callerSignal?.reason);
		if (callerSignal?.aborted) controller.abort(callerSignal.reason);
		else callerSignal?.addEventListener('abort', onCallerAbort, { once: true });
		let timedOut = false;
		const timer = setTimeout(() => {
			timedOut = true;
			controller.abort(new DOMException(`request timed out after ${API_REQUEST_TIMEOUT_MS}ms`, 'TimeoutError'));
		}, API_REQUEST_TIMEOUT_MS);
		try {
			const response = await this.fetchFn(`${this.baseUrl}${path}`, {
				...init,
				signal: controller.signal,
				headers,
				credentials: 'include'
			});
			if (!response.ok) {
				// Read once (capped at 1 MiB) so the paywall and typed error
				// share the structured contract.
				let body: unknown = null;
				try {
					body = await readCappedJson(response);
				} catch (error) {
					// A body read cut short by the deadline surfaces the
					// timeout, not a status default.
					if (timedOut) throw this.timeoutError();
					// An over-cap error body is never parsed: the generic
					// status fallback applies, never retryable (an over-cap
					// body does not become acceptable on retry), with no
					// paywall model emitted.
					if (isJsonBodyTooLarge(error)) {
						const parsed = parseErrorBody(null, response.status);
						throw new ApiError(response.status, parsed.code, parsed.message, null, false);
					}
					/* non-JSON error body: use status-based defaults */
				}
				const parsed = parseErrorBody(body, response.status);
				const paywall = paywallFromResponse({ status: response.status, headers: response.headers, body });
				throw new ApiError(response.status, parsed.code, parsed.message, paywall, parsed.retryable);
			}
			try {
				return { data: (await readCappedJson(response)) as T, status: response.status };
			} catch (error) {
				if (timedOut) throw this.timeoutError();
				// An over-cap 2xx never parses: surface the typed error, never
				// a bare SyntaxError, never retryable, never a paywall, and
				// never leaking body bytes (generic status message).
				if (isJsonBodyTooLarge(error)) {
					const parsed = parseErrorBody(null, response.status);
					throw new ApiError(response.status, parsed.code, parsed.message, null, false);
				}
				// A 2xx whose body is not JSON is a backend contract violation,
				// not a usable result: surface the typed error, never a bare
				// SyntaxError. Same generic fallback as undecodable error
				// bodies, but never retryable and never a paywall. Only decode
				// failures are rewrapped: transport errors (TypeError/AbortError
				// from a mid-body stream failure) rethrow untouched so they stay
				// distinguishable and retryable.
				if (!(error instanceof SyntaxError)) throw error;
				const parsed = parseErrorBody(null, response.status);
				throw new ApiError(response.status, parsed.code, parsed.message, null, false);
			}
		} catch (error) {
			if (error instanceof ApiError) throw error;
			// Only the deadline maps to the typed timeout error: caller
			// aborts and network failures rethrow untouched.
			if (timedOut) throw this.timeoutError();
			throw error;
		} finally {
			clearTimeout(timer);
			callerSignal?.removeEventListener('abort', onCallerAbort);
		}
	}

	/**
	 * Typed deadline failure: retryable (transient), never a paywall or an
	 * auth/plan classification. Status 504 keeps `retryableFromStatus`
	 * consistent for any caller that derives policy from status.
	 */
	private timeoutError(): ApiError {
		return new ApiError(
			504,
			'request_timeout',
			`request timed out after ${API_REQUEST_TIMEOUT_MS}ms deadline`,
			null,
			true
		);
	}
}
