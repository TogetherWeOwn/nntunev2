/**
 * Settings > ListenBrainz status and unlink flow, kept free of Svelte and the
 * DOM so every branch is unit-testable with a mocked `fetch`.
 *
 * Pre-PUT build: status and unlink only. Nothing here sends, stores or logs a
 * ListenBrainz token (the link route is not dispatched and the UI has no
 * token entry). The server response is always the source of truth: callers
 * never set "linked" optimistically.
 */

import { ApiError, type ApiClient, type ListenBrainzStatus } from '../api/client.js';
import { serializePublicKeyCredential, toRequestOptions } from '../webauthn.js';

/** Result of the status read; each kind maps to one section state. */
export type StatusOutcome =
	| { kind: 'linked'; username: string }
	| { kind: 'unlinked' }
	| { kind: 'signed_out' }
	| { kind: 'error' };

function fromStatus(status: ListenBrainzStatus): StatusOutcome {
	return status.linked ? { kind: 'linked', username: status.username } : { kind: 'unlinked' };
}

/** Any 401 that is not `reauth_required` means the session is gone. */
function isSignedOut(error: unknown): boolean {
	return error instanceof ApiError && error.status === 401 && error.code !== 'reauth_required';
}

export async function loadStatus(client: ApiClient, signal?: AbortSignal): Promise<StatusOutcome> {
	try {
		return fromStatus(await client.getListenBrainzStatus(signal));
	} catch (error) {
		return isSignedOut(error) ? { kind: 'signed_out' } : { kind: 'error' };
	}
}

/** Result of one DELETE attempt. */
export type UnlinkOutcome =
	| { kind: 'unlinked' }
	| { kind: 'reauth_required' }
	/** A second `reauth_required` after the one allowed retry: no dialog loop. */
	| { kind: 'reauth_again' }
	| { kind: 'signed_out' }
	| { kind: 'account_deleting' }
	| { kind: 'failed' };

/**
 * Send exactly one DELETE. Pass `afterReauth` for the single post-passkey
 * retry so a second `reauth_required` is reported as `reauth_again` instead of
 * reopening the dialog.
 */
export async function unlink(
	client: ApiClient,
	options: { afterReauth?: boolean; signal?: AbortSignal } = {}
): Promise<UnlinkOutcome> {
	try {
		const status = await client.unlinkListenBrainz(options.signal);
		// A 200 that still says linked is not an unlink: leave the UI as it was.
		return status.linked ? { kind: 'failed' } : { kind: 'unlinked' };
	} catch (error) {
		if (error instanceof ApiError) {
			if (error.code === 'reauth_required') {
				return { kind: options.afterReauth ? 'reauth_again' : 'reauth_required' };
			}
			if (error.code === 'account_deleting') return { kind: 'account_deleting' };
			if (error.status === 401) return { kind: 'signed_out' };
		}
		return { kind: 'failed' };
	}
}

export type PasskeyOutcome =
	| 'confirmed'
	/** Cancelled by the caller's signal or dismissed in the browser. */
	| 'dismissed'
	| 'failed';

/**
 * The existing reauthentication ceremony: `reauthenticateStart` ->
 * `navigator.credentials.get` -> `reauthenticateFinish`. Passkey only, with no
 * fallback to magic link, email or password.
 */
export async function confirmWithPasskey(
	client: ApiClient,
	signal: AbortSignal
): Promise<PasskeyOutcome> {
	try {
		const options = await client.reauthenticateStart(signal);
		if (signal.aborted) return 'dismissed';
		const assertion = await navigator.credentials.get({
			signal,
			publicKey: toRequestOptions({
				challenge: options.challenge,
				rpId: options.rp_id,
				allowCredentials: []
			})
		});
		if (signal.aborted) return 'dismissed';
		if (!(assertion instanceof PublicKeyCredential)) return 'failed';
		const serialized = serializePublicKeyCredential(assertion);
		const response = serialized.response as Record<string, unknown>;
		const { authenticatorData, clientDataJSON, signature } = response;
		const credentialId = serialized.id;
		if (
			typeof authenticatorData !== 'string' ||
			typeof clientDataJSON !== 'string' ||
			typeof signature !== 'string' ||
			typeof credentialId !== 'string'
		) {
			return 'failed';
		}
		await client.reauthenticateFinish(
			{
				challenge: options.challenge,
				credentialId,
				authenticatorData,
				clientDataJson: clientDataJSON,
				signature
			},
			signal
		);
		return signal.aborted ? 'dismissed' : 'confirmed';
	} catch (error) {
		if (signal.aborted) return 'dismissed';
		const name = (error as { name?: unknown } | null)?.name;
		if (!(error instanceof ApiError) && (name === 'NotAllowedError' || name === 'AbortError')) {
			return 'dismissed';
		}
		return 'failed';
	}
}
