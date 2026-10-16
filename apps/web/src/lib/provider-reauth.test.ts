import { describe, expect, it } from 'vitest';

import {
	INDEXER_AUTH_EXPIRED_CODE,
	PROVIDER_AUTH_EXPIRED_CODE,
	isIndexerAuthExpired,
	isProviderAuthExpired,
	reauthPromptFor
} from './provider-reauth.js';

describe('provider re-auth prompt', () => {
	it('documents the provider_auth_expired code', () => {
		expect(PROVIDER_AUTH_EXPIRED_CODE).toBe('provider_auth_expired');
	});

	it('recognises the re-auth code and nothing else', () => {
		expect(isProviderAuthExpired({ code: 'provider_auth_expired' })).toBe(true);
		expect(isProviderAuthExpired({ code: 'unauthorized' })).toBe(false);
		expect(isProviderAuthExpired({ code: 'request_failed' })).toBe(false);
		expect(isProviderAuthExpired(null)).toBe(false);
	});

	it('renders an inline re-auth prompt on that code, not a generic error page', () => {
		const prompt = reauthPromptFor('provider_auth_expired');
		expect(prompt).not.toBeNull();
		expect(prompt!).toMatch(/re-?enter|expired|password/i);
		expect(prompt!).not.toMatch(/s3cret/);
	});

	it('keeps the indexer code distinct and asks for the API key, not the password', () => {
		expect(INDEXER_AUTH_EXPIRED_CODE).toBe('indexer_auth_expired');
		expect(isIndexerAuthExpired({ code: 'indexer_auth_expired' })).toBe(true);
		expect(isProviderAuthExpired({ code: 'indexer_auth_expired' })).toBe(false);
		expect(isIndexerAuthExpired({ code: 'provider_auth_expired' })).toBe(false);
		const prompt = reauthPromptFor('indexer_auth_expired');
		expect(prompt).not.toBeNull();
		expect(prompt!).toMatch(/indexer|API key/i);
		expect(prompt!).not.toMatch(/re-?enter it below/i);
	});

	it('returns null for generic failures', () => {
		expect(reauthPromptFor('unauthorized')).toBeNull();
		expect(reauthPromptFor('request_failed')).toBeNull();
	});
});
