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

	/** Start passkey enrollment; the API also mails the inbox-proof code. */
	async registerStart(email: string): Promise<RegisterStartResponse> {
		return this.post<RegisterStartResponse>('/v1/auth/passkey/register/start', { email });
	}

	/** Finish passkey enrollment with the attestation + emailed code. */
	async registerFinish(input: RegisterFinishInput): Promise<MagicLinkResult> {
		return this.post<MagicLinkResult>(
			'/v1/auth/passkey/register/finish',
			toRegisterFinishBody(input)
		);
	}

	/** Start passkey login for an email (empty allow-list response). */
	async loginStart(email: string): Promise<LoginStartResponse> {
		return this.post<LoginStartResponse>('/v1/auth/passkey/login/start', { email });
	}

	/** Finish passkey login with the assertion. */
	async loginFinish(input: LoginFinishInput): Promise<MagicLinkResult> {
		return this.post<MagicLinkResult>(
			'/v1/auth/passkey/login/finish',
			toLoginFinishBody(input)
		);
	}

	/** Start a passkey proof bound to the currently authenticated session. */
	async reauthenticateStart(): Promise<LoginStartResponse> {
		return this.post<LoginStartResponse>('/v1/session/reauthenticate/start', {});
	}

	/** Complete proof-bound reauthentication without affecting ordinary login. */
	async reauthenticateFinish(input: LoginFinishInput): Promise<MagicLinkResult> {
		return this.post<MagicLinkResult>(
			'/v1/session/reauthenticate/finish',
			toLoginFinishBody(input)
		);
	}

	/**
	 * Live-test a provider (+ optional indexer) config without storing it.
	 * The API validates (SSRF guard), dials, and returns a redacted result —
	 * secrets never come back in the response.
	 */
	async testProviderConnection(input: ProviderTestInput): Promise<ProviderTestResult> {
		return this.post<ProviderTestResult>('/v1/vault/test', toVaultSaveBody(input));
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
				// Read once so the paywall and typed error share the structured contract.
				let body: unknown = null;
				try {
					body = await response.json();
				} catch {
					// A body read cut short by the deadline surfaces the
					// timeout, not a status default; anything else keeps the
					// existing non-JSON fallback below.
					if (timedOut) throw this.timeoutError();
					/* non-JSON error body: use status-based defaults */
				}
				const parsed = parseErrorBody(body, response.status);
				const paywall = paywallFromResponse({ status: response.status, headers: response.headers, body });
				throw new ApiError(response.status, parsed.code, parsed.message, paywall, parsed.retryable);
			}
			try {
				return (await response.json()) as T;
			} catch (error) {
				if (timedOut) throw this.timeoutError();
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
