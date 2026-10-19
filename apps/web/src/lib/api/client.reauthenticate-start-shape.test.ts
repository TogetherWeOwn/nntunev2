import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { ApiClient, ApiError } from './client.js';

// Builder-owned pin: `reauthenticateStart` shares the login-start success
// shape, so it must reject malformed `challenge` / `rp_id` bodies with the
// same typed `request_failed` ApiError before any ceremony can begin. This
// mirrors the protected `loginStart` matrix without touching that spec.

const BODY_SENTINEL = 'REAUTH-START-SHAPE-8f3c';

const VALID_REAUTH = {
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

describe('reauthenticateStart rejects malformed challenge/rp_id fields', () => {
	it.each(
		(['challenge', 'rp_id'] as const).flatMap((field) =>
			MALFORMED_VARIANTS.map((variant) => ({ field, variant }))
		)
	)('rejects a $variant $field before any credentials call', async ({ field, variant }) => {
		const client = clientReturning(
			JSON.stringify(malformedBody(VALID_REAUTH, field, variant))
		);

		const failure = await client
			.reauthenticateStart()
			.catch((error: unknown) => error);

		expectTypedShapeFailure(failure);
	});

	it('rejects a null body', async () => {
		const client = clientReturning('null');

		const failure = await client
			.reauthenticateStart()
			.catch((error: unknown) => error);

		expectTypedShapeFailure(failure);
	});

	it('rejects an array body', async () => {
		const client = clientReturning(JSON.stringify([VALID_REAUTH]));

		const failure = await client
			.reauthenticateStart()
			.catch((error: unknown) => error);

		expectTypedShapeFailure(failure);
	});
});
