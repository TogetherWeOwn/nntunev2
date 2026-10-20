import { describe, expect, it } from 'vitest';
import {
	validateIndexerFields,
	validateIndexerUrl,
	validateProviderConfig,
	validateProviderFields,
	type ProviderForm
} from './vault-validation.js';

const provider: ProviderForm = {
	host: 'news.example.test', port: 563, tls: true, allowInsecurePlaintext: false,
	username: 'reader', password: 'synthetic-secret', maxConnections: 4
};

describe('field validation relationships', () => {
	it('does not associate errors with valid fields', () => {
		expect(validateProviderFields(provider)).toEqual({});
		expect(validateIndexerFields('', '')).toEqual({});
		expect(validateIndexerFields('https://index.example.test/api', 'synthetic-key')).toEqual({});
	});

	it.each([
		['host', 'https://news.example.test/', /hostname.*scheme/i],
		['port', 123, /119 or 563/],
		['username', ' ', /username/],
		['username', 'é'.repeat(128), /255 UTF-8 bytes/],
		['password', '', /password/],
		['password', 'é'.repeat(513), /1024 UTF-8 bytes/],
		['maxConnections', 0, /Connections/]
	] as const)('attributes %s validation without affecting other fields', (field, value, message) => {
		const form = { ...provider, [field]: value };
		expect(validateProviderFields(form)).toEqual({ [field]: expect.stringMatching(message) });
		expect(Object.values(validateProviderFields(form))).toEqual(validateProviderConfig(form));
	});

	it('associates plaintext consent with the acknowledgement and preserves error order', () => {
		const form = { ...provider, host: 'invalid', tls: false, username: ' ', maxConnections: 51 };
		const errors = validateProviderFields(form);
		expect(Object.keys(errors)).toEqual(['host', 'allowInsecurePlaintext', 'username', 'maxConnections']);
		expect(errors.allowInsecurePlaintext).toMatch(/acknowledgement/);
		expect(Object.values(errors)).toEqual(validateProviderConfig(form));
		expect(validateProviderFields({ ...provider, tls: false, allowInsecurePlaintext: true })).toEqual({});
	});

	it.each([
		['http://index.example.test/api', 'synthetic-key', 'indexerUrl', /https/],
		['https://localhost/api', 'synthetic-key', 'indexerUrl', /public host/],
		['', 'synthetic-key', 'indexerUrl', /enter an indexer URL/],
		['https://index.example.test/api', '', 'indexerApiKey', /API key/],
		['https://index.example.test/api', 'é'.repeat(513), 'indexerApiKey', /1024 UTF-8 bytes/]
	])('attributes indexer errors for %s', (url, key, field, message) => {
		const errors = validateIndexerFields(url, key);
		expect(errors).toEqual({ [field]: expect.stringMatching(message) });
		expect(Object.values(errors)[0]).toEqual(validateIndexerUrl(url, key));
		if (key.length > 0) expect(Object.values(errors).join(' ')).not.toContain(key);
	});
});
