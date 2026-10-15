import { describe, expect, it } from 'vitest';

import {
	PROVIDER_AUTH_EXPIRED_CODE,
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

	it('returns null for generic failures', () => {
		expect(reauthPromptFor('unauthorized')).toBeNull();
		expect(reauthPromptFor('request_failed')).toBeNull();
	});
});
