import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { ApiClient, ApiError } from './client.js';

// Protected acceptance spec: `registerStart` / `loginStart` must validate the
// passkey-start challenge shape before any ceremony can begin.
//
// Pinned rules (the builder implements exactly these):
// 1. The register-start success body must be a plain object whose
//    `challenge`, `user_id` and `rp_id` are each a non-empty string. The
//    login-start success body must be a plain object whose `challenge` and
//    `rp_id` are each a non-empty string. A missing, empty-string or
//    non-string value for any of those fields — or a non-object body — is
//    NOT a successful start: it surfaces as a typed `ApiError` with code
//    `request_failed`, the real HTTP status, not retryable, no paywall, and
//    a message that does not leak the raw body. In particular an empty or
//    numeric `challenge` must never reach a caller, where it would explode
//    inside base64url decoding mid-ceremony as a generic ceremony error.
// 2. The rejection happens inside the start call itself: while a malformed
//    start body is being rejected, `navigator.credentials.create` /
//    `navigator.credentials.get` are never called. The ceremony only ever
//    runs on a shape-valid start result.
// 3. Valid start bodies flow unchanged: a register triple, a login pair
//    (with its `allow_credentials` list), and a session reauthentication
//    start all resolve exactly as received, and non-2xx responses keep
//    their existing typed-error semantics.

const BODY_SENTINEL = 'PASSKEY-START-SHAPE-5d2b';

const VALID_REGISTER = {
	challenge: 'AQIDBAUG',
	user_id: 'BwgJCgs',
	rp_id: 'example.com'
};

const VALID_LOGIN = {
	challenge: 'AQIDBAUG',
	rp_id: 'example.com',
	allow_credentials: [] as unknown[]
};

const credentialsCreate = vi.fn(async () => null);
const credentialsGet = vi.fn(async () => null);

beforeEach(() => {
	credentialsCreate.mockClear();
	credentialsGet.mockClear();
	vi.stubGlobal('navigator', {
		credentials: { create: credentialsCreate, get: credentialsGet }
	});
});

afterEach(() => {
	vi.unstubAllGlobals();
});

function clientReturning(body: string, status = 200): ApiClient {
	const fetchFn = vi.fn(
		async () =>
			new Response(body, {
				status,
				headers: { 'content-type': 'application/json' }
			})
	);
	return new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });
}

function expectTypedShapeFailure(failure: unknown, status = 200): void {
	expect(failure).toBeInstanceOf(ApiError);
	const apiError = failure as ApiError;
	expect(apiError.name).toBe('ApiError');
	expect(apiError.status).toBe(status);
	expect(apiError.code).toBe('request_failed');
	expect(apiError.retryable).toBe(false);
	expect(apiError.paywall).toBeNull();
	expect(apiError.message.length).toBeGreaterThan(0);
	expect(apiError.message).not.toContain(BODY_SENTINEL);
	expect(credentialsCreate).not.toHaveBeenCalled();
	expect(credentialsGet).not.toHaveBeenCalled();
}

function withoutField(
	body: Record<string, unknown>,
	field: string
): Record<string, unknown> {
	const copy: Record<string, unknown> = { ...body, note: BODY_SENTINEL };
	delete copy[field];
	return copy;
}

function withField(
	body: Record<string, unknown>,
	field: string,
	value: unknown
): Record<string, unknown> {
	return { ...body, note: BODY_SENTINEL, [field]: value };
}

const MALFORMED_VARIANTS = ['missing', 'empty', 'number', 'null'] as const;
type MalformedVariant = (typeof MALFORMED_VARIANTS)[number];

function malformedBody(
	valid: Record<string, unknown>,
	field: string,
	variant: MalformedVariant
): Record<string, unknown> {
	switch (variant) {
		case 'missing':
			return withoutField(valid, field);
		case 'empty':
			return withField(valid, field, '');
		case 'number':
			return withField(valid, field, 42);
		case 'null':
			return withField(valid, field, null);
	}
}

describe('registerStart rejects malformed challenge/user_id/rp_id fields', () => {
	it.each(
		(['challenge', 'user_id', 'rp_id'] as const).flatMap((field) =>
			MALFORMED_VARIANTS.map((variant) => ({ field, variant }))
		)
	)('rejects a $variant $field before any credentials call', async ({ field, variant }) => {
		const client = clientReturning(
			JSON.stringify(malformedBody(VALID_REGISTER, field, variant))
		);

		const failure = await client
			.registerStart('ada@example.com')
			.catch((error: unknown) => error);

		expectTypedShapeFailure(failure);
	});

	it.each([
		{ label: 'a boolean', value: true },
		{ label: 'an object', value: { raw: 'AQID' } },
		{ label: 'an array', value: ['AQID'] }
	])('rejects $label challenge before any credentials call', async ({ value }) => {
		const client = clientReturning(
			JSON.stringify(withField(VALID_REGISTER, 'challenge', value))
		);

		const failure = await client
			.registerStart('ada@example.com')
			.catch((error: unknown) => error);

		expectTypedShapeFailure(failure);
	});

	it('rejects a malformed register body at 201 with the real status, not 200', async () => {
		const client = clientReturning(
			JSON.stringify(withField(VALID_REGISTER, 'challenge', '')),
			201
		);

		const failure = await client
			.registerStart('ada@example.com')
			.catch((error: unknown) => error);

		expectTypedShapeFailure(failure, 201);
	});
});

describe('loginStart rejects malformed challenge/rp_id fields', () => {
	it.each(
		(['challenge', 'rp_id'] as const).flatMap((field) =>
			MALFORMED_VARIANTS.map((variant) => ({ field, variant }))
		)
	)('rejects a $variant $field before any credentials call', async ({ field, variant }) => {
		const client = clientReturning(
			JSON.stringify(malformedBody(VALID_LOGIN, field, variant))
		);

		const failure = await client
			.loginStart('ada@example.com')
			.catch((error: unknown) => error);

		expectTypedShapeFailure(failure);
	});

	it.each([
		{ label: 'a boolean', value: false },
		{ label: 'an array', value: ['AQID'] }
	])('rejects $label challenge before any credentials call', async ({ value }) => {
		const client = clientReturning(
			JSON.stringify(withField(VALID_LOGIN, 'challenge', value))
		);

		const failure = await client
			.loginStart('ada@example.com')
			.catch((error: unknown) => error);

		expectTypedShapeFailure(failure);
	});
});

describe('passkey-start rejects non-object success bodies', () => {
	it('rejects a null register body', async () => {
		const client = clientReturning('null');

		const failure = await client
			.registerStart('ada@example.com')
			.catch((error: unknown) => error);

		expectTypedShapeFailure(failure);
	});

	it('rejects an array register body', async () => {
		const client = clientReturning(JSON.stringify([VALID_REGISTER]));

		const failure = await client
			.registerStart('ada@example.com')
			.catch((error: unknown) => error);

		expectTypedShapeFailure(failure);
	});

	it('rejects a bare string register body without leaking it', async () => {
		const client = clientReturning(JSON.stringify(BODY_SENTINEL));

		const failure = await client
			.registerStart('ada@example.com')
			.catch((error: unknown) => error);

		expectTypedShapeFailure(failure);
	});

	it('rejects a null login body', async () => {
		const client = clientReturning('null');

		const failure = await client
			.loginStart('ada@example.com')
			.catch((error: unknown) => error);

		expectTypedShapeFailure(failure);
	});

	it('rejects an array login body', async () => {
		const client = clientReturning(JSON.stringify([VALID_LOGIN]));

		const failure = await client
			.loginStart('ada@example.com')
			.catch((error: unknown) => error);

		expectTypedShapeFailure(failure);
	});

	it('rejects an empty-object login body (every field missing)', async () => {
		const client = clientReturning(JSON.stringify({}));

		const failure = await client
			.loginStart('ada@example.com')
			.catch((error: unknown) => error);

		expectTypedShapeFailure(failure);
	});
});

describe('passkey-start valid-flow controls', () => {
	it('passes a valid register triple through unchanged', async () => {
		const client = clientReturning(JSON.stringify(VALID_REGISTER));

		await expect(client.registerStart('ada@example.com')).resolves.toEqual(
			VALID_REGISTER
		);
		expect(credentialsCreate).not.toHaveBeenCalled();
		expect(credentialsGet).not.toHaveBeenCalled();
	});

	it('passes a valid login pair with an empty allow-list through unchanged', async () => {
		const client = clientReturning(JSON.stringify(VALID_LOGIN));

		await expect(client.loginStart('ada@example.com')).resolves.toEqual(VALID_LOGIN);
		expect(credentialsCreate).not.toHaveBeenCalled();
		expect(credentialsGet).not.toHaveBeenCalled();
	});

	it('keeps server-provided allow_credentials entries on a valid login start', async () => {
		const body = {
			...VALID_LOGIN,
			allow_credentials: [{ id: 'BAUG', transports: ['internal'] }]
		};
		const client = clientReturning(JSON.stringify(body));

		await expect(client.loginStart('ada@example.com')).resolves.toEqual(body);
	});

	it('passes a valid reauthentication start through unchanged', async () => {
		const client = clientReturning(JSON.stringify(VALID_LOGIN));

		await expect(client.reauthenticateStart()).resolves.toEqual(VALID_LOGIN);
		expect(credentialsCreate).not.toHaveBeenCalled();
		expect(credentialsGet).not.toHaveBeenCalled();
	});

	it('preserves the typed 401 for registerStart instead of a shape failure', async () => {
		const client = clientReturning(JSON.stringify({ error: 'unauthorized' }), 401);

		const failure = await client
			.registerStart('ada@example.com')
			.catch((error: unknown) => error);

		expect(failure).toBeInstanceOf(ApiError);
		expect((failure as ApiError).status).toBe(401);
		expect((failure as ApiError).code).toBe('unauthorized');
		expect(credentialsCreate).not.toHaveBeenCalled();
		expect(credentialsGet).not.toHaveBeenCalled();
	});

	it('preserves the typed 401 for loginStart instead of a shape failure', async () => {
		const client = clientReturning(JSON.stringify({ error: 'unauthorized' }), 401);

		const failure = await client
			.loginStart('ada@example.com')
			.catch((error: unknown) => error);

		expect(failure).toBeInstanceOf(ApiError);
		expect((failure as ApiError).status).toBe(401);
		expect((failure as ApiError).code).toBe('unauthorized');
		expect(credentialsCreate).not.toHaveBeenCalled();
		expect(credentialsGet).not.toHaveBeenCalled();
	});
});
