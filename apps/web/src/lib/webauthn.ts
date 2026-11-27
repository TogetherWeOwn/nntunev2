/**
 * WebAuthn wire-format helpers shared by the signup and login pages.
 *
 * The API speaks base64url (no padding); the `navigator.credentials` API
 * wants raw bytes (`BufferSource`). Centralized here so both pages decode
 * identically and send portable JSON in browsers with or without `toJSON()`.
 */

export interface CredentialDescriptorJSON {
	id: string;
	type?: PublicKeyCredentialType;
	transports?: AuthenticatorTransport[];
}

export interface RegistrationOptionsJSON {
	challenge: string;
	rp: PublicKeyCredentialRpEntity;
	user: Omit<PublicKeyCredentialUserEntity, 'id'> & { id: string };
	pubKeyCredParams: PublicKeyCredentialParameters[];
	timeout?: number;
	excludeCredentials?: CredentialDescriptorJSON[];
	authenticatorSelection?: AuthenticatorSelectionCriteria;
	attestation?: AttestationConveyancePreference;
	extensions?: AuthenticationExtensionsClientInputs;
}

export interface AuthenticationOptionsJSON {
	challenge: string;
	rpId?: string;
	timeout?: number;
	allowCredentials?: CredentialDescriptorJSON[];
	userVerification?: UserVerificationRequirement;
	extensions?: AuthenticationExtensionsClientInputs;
}

/** Decode canonical, unpadded base64url to raw bytes. */
export function decodeBase64Url(value: string): Uint8Array<ArrayBuffer> {
	if (typeof value !== 'string' || /[^A-Za-z0-9_-]/.test(value)) throw new TypeError('Invalid base64url');
	const base64 = value.replace(/-/g, '+').replace(/_/g, '/');
	const binary = atob(base64.padEnd(base64.length + ((4 - (base64.length % 4)) % 4), '='));
	const bytes = Uint8Array.from(binary, (char) => char.charCodeAt(0));
	if (encodeBase64Url(bytes) !== value) throw new TypeError('Invalid base64url');
	return bytes;
}

/** Encode an ArrayBuffer or view as unpadded base64url. */
export function encodeBase64Url(value: ArrayBuffer | ArrayBufferView): string {
	const bytes =
		value instanceof ArrayBuffer
			? new Uint8Array(value)
			: new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
	let binary = '';
	for (const byte of bytes) binary += String.fromCharCode(byte);
	return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/g, '');
}

/**
 * Build API JSON creation options from the Worker's `register/start`
 * response (`{challenge, user_id, rp_id}`). The server deliberately returns
 * only these three fields; the client assembles the full WebAuthn options
 * (algorithms the server verifies: ES256 and EdDSA; `none` attestation,
 * which the server accepts). Shape pinned by
 * `workers/api/tests/fixtures/api-web-contract.json`.
 * The public user_id is not an authenticator handle. Generate a fresh handle
 * for every creation attempt, including retries with the same challenge.
 */
export function toRegistrationOptions(
	challenge: string,
	rpId: string,
	email: string
): RegistrationOptionsJSON {
	const handle = crypto.getRandomValues(new Uint8Array(32));
	return {
		challenge,
		rp: { name: 'NNTune', id: rpId },
		user: { id: encodeBase64Url(handle), name: email, displayName: email },
		pubKeyCredParams: [
			{ type: 'public-key', alg: -7 },
			{ type: 'public-key', alg: -8 }
		],
		excludeCredentials: [],
		authenticatorSelection: {
			residentKey: 'required',
			requireResidentKey: true,
			userVerification: 'required'
		},
		attestation: 'none'
	};
}

/**
 * Convert API JSON creation options into browser options, requiring
 * discoverable credentials and user verification (PIN/biometric). The Worker
 * rejects any ceremony whose authenticatorData lacks the UV flag, so the
 * browser is asked for it up front instead of failing after the tap.
 */
export function toCreationOptions(
	options: RegistrationOptionsJSON
): PublicKeyCredentialCreationOptions {
	const { challenge, user, excludeCredentials, authenticatorSelection, ...rest } = options;
	return {
		...rest,
		challenge: decodeChallenge(challenge),
		user: { ...user, id: decodeField(user.id, 64) },
		excludeCredentials: toCredentialList(excludeCredentials),
		authenticatorSelection: {
			...authenticatorSelection,
			residentKey: 'required',
			requireResidentKey: true,
			userVerification: 'required'
		}
	};
}

/**
 * Convert API JSON request options; omitted allowCredentials enables
 * usernameless login. User verification is always required (see
 * `toCreationOptions`); a caller-supplied weaker value is overridden.
 */
export function toRequestOptions(
	options: AuthenticationOptionsJSON
): PublicKeyCredentialRequestOptions {
	const { challenge, allowCredentials, ...rest } = options;
	return {
		...rest,
		challenge: decodeChallenge(challenge),
		userVerification: 'required',
		...(allowCredentials === undefined
			? {}
			: { allowCredentials: toCredentialList(allowCredentials) })
	};
}

function toCredentialDescriptor(
	descriptor: CredentialDescriptorJSON
): PublicKeyCredentialDescriptor {
	if (descriptor.type !== undefined && descriptor.type !== 'public-key') {
		throw new TypeError('Invalid WebAuthn credential type');
	}
	return {
		id: decodeField(descriptor.id, MAX_CREDENTIAL_ID),
		type: 'public-key',
		transports: descriptor.transports
	};
}

/** Serialize credential response buffers without relying on newer `toJSON()` support. */
export function serializePublicKeyCredential(
	credential: PublicKeyCredential
): Record<string, unknown> {
	if (credential.type !== 'public-key' || typeof credential.id !== 'string' ||
		!credential.id.length || credential.id.length > Math.ceil(MAX_CREDENTIAL_ID * 4 / 3) ||
		/[^A-Za-z0-9_-]/.test(credential.id)) throw new TypeError('Invalid public-key credential');
	const rawId = encodeField(credential.rawId, MAX_CREDENTIAL_ID);
	const response = credential.response;
	const serializedResponse: Record<string, unknown> = {
		clientDataJSON: encodeField(response.clientDataJSON, MAX_RESPONSE_BYTES)
	};

	if ('attestationObject' in response) {
		const attestation = response as AuthenticatorAttestationResponse;
		serializedResponse.attestationObject = encodeField(attestation.attestationObject, MAX_RESPONSE_BYTES);
		if (typeof attestation.getTransports === 'function') {
			serializedResponse.transports = attestation.getTransports();
		}
	} else {
		const assertion = response as AuthenticatorAssertionResponse;
		serializedResponse.authenticatorData = encodeField(assertion.authenticatorData, MAX_RESPONSE_BYTES);
		serializedResponse.signature = encodeField(assertion.signature, 1024);
		serializedResponse.userHandle =
			assertion.userHandle === null ? null : encodeField(assertion.userHandle, 64);
	}

	return {
		id: credential.id,
		rawId,
		type: credential.type,
		authenticatorAttachment: credential.authenticatorAttachment,
		response: serializedResponse,
		clientExtensionResults: credential.getClientExtensionResults()
	};
}

const MAX_CREDENTIAL_ID = 1024;
const MAX_RESPONSE_BYTES = 16 * 1024;

function toCredentialList(descriptors: CredentialDescriptorJSON[] | undefined) {
	if (descriptors === undefined) return;
	if (!Array.isArray(descriptors) || descriptors.length > 64) throw new TypeError('Invalid WebAuthn credential list');
	return Array.from(descriptors, toCredentialDescriptor);
}

function decodeField(value: string, maxBytes: number): Uint8Array<ArrayBuffer> {
	if (typeof value !== 'string' || !value.length || value.length > Math.ceil(maxBytes * 4 / 3)) {
		throw new TypeError('Invalid WebAuthn field length');
	}
	return decodeBase64Url(value);
}

function decodeChallenge(value: string): Uint8Array<ArrayBuffer> {
	// Accept correctly padded challenges at this boundary for existing callers.
	if (typeof value === 'string' && value.length <= 44 && value.length % 4 === 0) {
		value = value.replace(/={1,2}$/, '');
	}
	return decodeField(value, 32);
}

function encodeField(value: ArrayBuffer, maxBytes: number): string {
	if (!(value instanceof ArrayBuffer) || !value.byteLength || value.byteLength > maxBytes) {
		throw new TypeError('Invalid WebAuthn response buffer');
	}
	return encodeBase64Url(value);
}
