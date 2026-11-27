import { describe, expect, it, vi } from 'vitest';
import { render } from 'svelte/server';
import { parse } from 'svelte/compiler';
import ts from 'typescript';
import { BUILD_SHA } from '#lib/build.js';
import { APP_VERSION } from '#lib/version.js';
import * as build from './api/build/+server.js';
import * as health from './api/health/+server.js';
import * as ready from './api/ready/+server.js';
import * as manifest from './manifest.webmanifest/+server.js';
import * as home from './home/+page.js';
import * as library from './library/+page.js';
import * as liked from './library/liked/+page.js';
import * as recent from './library/recent/+page.js';
import * as search from './search/+page.js';
import Search from './search/+page.svelte';
import Login from './login/+page.svelte';
import Signup from './signup/+page.svelte';
import { takeMagicToken } from './auth/magic/+page.svelte';

const authPages = import.meta.glob<string>('./{auth/magic,login,signup}/+page.svelte', {
	query: '?raw', import: 'default', eager: true
});

// Execute the real route handlers with local doubles, following the shortcut
// handler test pattern. Client hydration and DOM event dispatch are outside this test.
function authHandler(route: string, name: string, bindings: Record<string, unknown>, helpers: string[] = []) {
	const source = authPages[`./${route}/+page.svelte`];
	const { instance } = parse(source);
	if (!instance) throw new Error(`Missing script in ${route}`);
	const script = ts.createSourceFile('page.ts', source.slice(instance.content.start, instance.content.end),
		ts.ScriptTarget.Latest, true);
	const functions = [name, ...helpers].map((key) => {
		const declaration = script.statements.find((node) => ts.isFunctionDeclaration(node) && node.name?.text === key);
		if (!declaration) throw new Error(`Missing ${key} in ${route}`);
		return declaration.getText(script);
	});
	const { outputText } = ts.transpileModule(functions.join('\n'), { compilerOptions: { target: ts.ScriptTarget.ES2022 } });
	return new Function(...Object.keys(bindings), `${outputText}\nreturn ${name};`)(...Object.values(bindings)) as
		(event?: { preventDefault(): void }) => Promise<void>;
}

describe('operational and install responses', () => {
	it.each([
		{ route: build, body: { sha: BUILD_SHA } },
		{ route: health, body: { ok: true, version: APP_VERSION } },
		{ route: ready, body: { ok: true } }
	])('serves uncached runtime JSON with the declared fields', async ({ route, body }) => {
		expect(route.prerender).toBe(false);
		const response = await route.GET({} as never);
		expect(response.status).toBe(200);
		expect(response.headers.get('content-type')).toBe('application/json');
		expect(response.headers.get('cache-control')).toBe('no-store');
		expect(await response.json()).toEqual(body);
	});

	it('serves a public same-origin install manifest', async () => {
		const response = await manifest.GET({} as never);
		expect(response.status).toBe(200);
		expect(response.headers.get('content-type')).toBe('application/manifest+json');
		expect(response.headers.get('cache-control')).toBe('public, max-age=3600');
		expect(response.headers.has('set-cookie')).toBe(false);
		expect(await response.json()).toMatchObject({
			start_url: '/', scope: '/', icons: [
				{ src: '/icons/icon-192.png' }, { src: '/icons/icon-512.png' }
			]
		});
	});
});

describe('client-only route loaders', () => {
	it('keeps account routes off SSR and their page modules free of loads', () => {
		for (const route of [home, library, liked, recent]) {
			expect(route.ssr).toBe(false);
			expect(route).not.toHaveProperty('load');
		}
		expect(search.ssr).toBe(false);
	});

	it.each([
		['', ''],
		['?q=++Caf%C3%A9%2B+%F0%9F%8E%B5++', 'Café+ 🎵'],
		[`?q=${'a'.repeat(512)}`, 'a'.repeat(512)],
		[`?q=${'a'.repeat(513)}`, null],
		['?q=one&%71=two', null],
		['?q=%00artist', null]
	])('validates the applied search query %s', async (query, expected) => {
		const url = new URL(`/search${query}`, 'https://example.test');
		expect(await search.load({ url } as Parameters<typeof search.load>[0])).toMatchObject({ query: expected });
	});

	it.each([false, true])('renders an editable rejected query (truncated=%s)', async (truncated) => {
		const value = truncated ? `${'a'.repeat(2047)}🎵end` : 'a'.repeat(520);
		const url = new URL(`/search?${new URLSearchParams({ q: value })}`, 'https://example.test');
		const data = await search.load({ url } as Parameters<typeof search.load>[0]);
		const draft = truncated ? 'a'.repeat(2047) : value;
		expect(data).toEqual({ query: null, recoveryDraft: draft, draftTruncated: truncated });
		const { body } = render(Search, { props: { data, params: {} } });
		expect(body).toContain(`value="${draft}"`);
		expect(body.includes('Only the start of your search is shown.')).toBe(truncated);
	});

	it('refuses a lone surrogate in a draft before URL serialization', () => {
		expect(search._normalizeQuery('\ud800')).toBeNull();
	});
});

describe('credential inputs', () => {
	it.each([Login, Signup])('submits email forms as POST with an email limit', (Component) => {
		const { body } = render(Component);
		expect(body).toMatch(/<form\b[^>]*\bmethod="post"[^>]*>[\s\S]*?<input\b[^>]*\btype="email"[^>]*\bmaxlength="254"/);
	});

	it.each([
		['?token=synthetic-link_1', 'synthetic-link_1'],
		['', null],
		['?token=', null],
		['?token=first&%74oken=second', null],
		['?token=synthetic%0A', null],
		['?token=synthetic%2Blink', null],
		[`?token=${'a'.repeat(257)}`, null]
	])('scrubs the magic-link URL and validates %s', (search, expected) => {
		const history = { replaceState: vi.fn() };
		expect(takeMagicToken({ pathname: '/auth/magic', search }, history)).toBe(expected);
		expect(history.replaceState).toHaveBeenCalledExactlyOnceWith(null, '', '/auth/magic');
	});

	it('refuses to retain a credential when browser history cannot be scrubbed', () => {
		const history = { replaceState: () => { throw new DOMException('History unavailable'); } };
		expect(takeMagicToken({ pathname: '/auth/magic', search: '?token=synthetic' }, history)).toBeNull();
	});

	// These handlers run after hydration. Pin the response guard between each
	// real request and its success side effect without starting a browser.
	it.each([
		['login', 'requestMagicLink', "status = 'sent'"],
		['login', 'loginFinish', 'stashSignedInEmail(attemptedEmail)']
	])('gates %s %s success effects on an affirmative result', (route, method, effect) => {
		const source = authPages[`./${route}/+page.svelte`];
		const request = source.indexOf(`await client.${method}(`);
		const success = source.indexOf(effect, request);
		expect(request).toBeGreaterThan(-1);
		expect(success).toBeGreaterThan(request);
		expect(source.slice(request, success)).toMatch(/if \(result\?\.ok !== true\) throw new Error\(/);
	});

	it.each([false, true])('gates magic-link success side effects (acknowledged=%s)', async (acknowledged) => {
		const invalidateLibraryCache = vi.fn();
		const stashSignedInEmail = vi.fn();
		const location = { href: '/auth/magic' };
		const finishMagicLink = vi.fn(async () => acknowledged ? { ok: true, email: 'member@example.test' } : {});
		const redeem = authHandler('auth/magic', 'redeem', {
			mounted: true, status: 'confirm', token: 'synthetic', redeemError: null,
			client: { finishMagicLink }, invalidateLibraryCache, stashSignedInEmail, window: { location }
		});
		await redeem();
		expect(finishMagicLink).toHaveBeenCalledExactlyOnceWith('synthetic');
		expect(invalidateLibraryCache).toHaveBeenCalledTimes(Number(acknowledged));
		expect(stashSignedInEmail).toHaveBeenCalledTimes(Number(acknowledged));
		expect(location.href).toBe(acknowledged ? '/library' : '/auth/magic');
	});

	const email = 'member@example.test';
	const attempt = new AbortController();
	const event = { preventDefault() {} };
	const emailInput = { reportValidity: () => true };
	const emailForm = { email, maxEmailLength: 254, emailInput, attempt, passkeysSupported: true };

	it.each([
		['sendMagicLink', 'requestMagicLink', 'idle', 'working'],
		['signInWithPasskey', 'loginStart', 'sending', 'idle']
	])('blocks %s while the other login mode is active', async (name, method, status, passkeyStatus) => {
		const request = vi.fn(() => { throw new Error('Unexpected request'); });
		await authHandler('login', name, {
			...emailForm, status, passkeyStatus, client: { [method]: request },
			ApiError: class extends Error {}, isPasskeyDismissal: () => false
		})(event);
		expect(request).not.toHaveBeenCalled();
	});

	it.each(['invite with spaces', 'i'.repeat(257), 'invite\u0000code'])('blocks a malformed signup invite before starting', async (inviteCode) => {
		const registerStart = vi.fn(() => { throw new Error('Unexpected request'); });
		await authHandler('signup', 'startAccount', {
			...emailForm, status: 'idle', maxCodeLength: 256, inviteCode, client: { registerStart }, ApiError: Error
		}, ['validInvite'])(event);
		expect(registerStart).not.toHaveBeenCalled();
	});

	it.each([
		{ magicCode: 'bad code', inviteCode: '', calls: 0 },
		{ magicCode: 'a'.repeat(257), inviteCode: '', calls: 0 },
		{ magicCode: 'synthetic', inviteCode: 'bad\u0000invite', calls: 0 },
		{ magicCode: 'synthetic', inviteCode: '', calls: 1 }
	])('gates signup ceremony and rejects an invalid private-listening reply', async ({ magicCode, inviteCode, calls }) => {
		class Credential {}
		const create = vi.fn(async () => new Credential());
		const registerFinish = vi.fn(async () => ({ ok: true, private_listening: false }));
		const invalidateLibraryCache = vi.fn();
		await authHandler('signup', 'finishAccount', {
			...emailForm, status: 'code-sent', maxCodeLength: 256, magicCode, inviteCode,
			pending: { challenge: 'synthetic', rp_id: 'example.test' }, finishLocked: false, privateListening: false,
			navigator: { credentials: { create } }, PublicKeyCredential: Credential,
			toCreationOptions: vi.fn(), toRegistrationOptions: vi.fn(),
			serializePublicKeyCredential: () => ({ id: 'synthetic', response: { attestationObject: 'data', clientDataJSON: 'data' } }),
			client: { registerFinish }, invalidateLibraryCache, ApiError: class extends Error {}, isPasskeyDismissal: () => false
		}, ['validInvite'])(event);
		expect(create).toHaveBeenCalledTimes(calls);
		expect(registerFinish).toHaveBeenCalledTimes(calls);
		expect(invalidateLibraryCache).not.toHaveBeenCalled();
	});
});
