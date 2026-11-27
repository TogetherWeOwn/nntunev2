import { describe, expect, it, vi } from 'vitest';
import { render } from 'svelte/server';
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
import Login from './login/+page.svelte';
import Signup from './signup/+page.svelte';
import { takeMagicToken } from './auth/magic/+page.svelte';

const authPages = import.meta.glob<string>('./{auth/magic,login,signup}/+page.svelte', {
	query: '?raw', import: 'default', eager: true
});

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
		expect(await search.load({ url } as Parameters<typeof search.load>[0])).toEqual({ query: expected });
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
		['auth/magic', 'finishMagicLink', 'invalidateLibraryCache()'],
		['login', 'requestMagicLink', "status = 'sent'"],
		['login', 'loginFinish', 'stashSignedInEmail(attemptedEmail)'],
		['signup', 'registerFinish', "status = 'created'"]
	])('gates %s %s success effects on an affirmative result', (route, method, effect) => {
		const source = authPages[`./${route}/+page.svelte`];
		const request = source.indexOf(`await client.${method}(`);
		const success = source.indexOf(effect, request);
		expect(request).toBeGreaterThan(-1);
		expect(success).toBeGreaterThan(request);
		expect(source.slice(request, success)).toContain('if (result?.ok !== true');
	});
});
