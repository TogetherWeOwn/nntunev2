import { expect, test, type Page } from '@playwright/test';

/**
 * Auth-transition acceptance: a qualifying security result (logout or
 * revoke-all fulfilled success, or a typed HTTP401) purges this tab's
 * accessible auth state and resets every mounted consumer — library and
 * account views — even when the security route was abandoned first.
 * The abandoned route itself stays silent: no navigation, error, message
 * or busy-state writes. Non-qualifying results notify nothing.
 *
 * Transport contract pinned here: one shared same-window dispatch plus the
 * existing localStorage transition for other tabs. Receivers never
 * rebroadcast, request, navigate or retry; delivery is best effort. Every
 * held request and event below is released explicitly; bounded waits cover
 * only negative observation, never race outcomes. All API destinations are
 * local fixtures, never live services.
 *
 * Breaks caught: an abandoned qualifying result that leaves snapshots or
 * identity labels behind; a late library response that repopulates a reset
 * view; a retained paywall retry that resubmits; a new paywall published
 * by a late response; a rebroadcast/request loop; a storage denial that
 * suppresses the local reset or throws.
 */
const SNAPSHOT = 'nntune.library.snapshot.v1';
const AUTH_CONTEXT = 'nntune.library.auth-context.v1';
const TRANSITION = 'nntune.library.auth-transition.v1';
const EMAIL = 'nntune:signed-in-email';

interface HeldRequest {
	method: string;
	path: string;
	body: unknown;
}

interface TransitionProbe {
	requests: HeldRequest[];
	rejections: string[];
	writes: string[];
	broadcasts: string[];
	release: (path: string, status: number, body: unknown) => void;
}

declare global {
	interface Window {
		__transition: TransitionProbe;
	}
}

type HoldRule = string | { path: string; methods?: string[]; passFirst?: number };

/**
 * Hold the fetch boundary for `holds`; the real clients, handlers,
 * Svelte teardown, routing and cache code all run. The held transport
 * deliberately ignores cancellation: a late response still needs a page
 * or client guard. Anything under /v1/** that is not held fails loudly so
 * a stray request cannot hide behind a benign fixture.
 *
 * A rule can narrow by HTTP method and can pass the first N matches
 * through (the security page load probes GET /v1/me/subscription before any
 * held action runs, so a subscription hold must not swallow that probe).
 */
async function fixtures(page: Page, holds: HoldRule[]) {
	const normalized = holds.map((rule) =>
		typeof rule === 'string'
			? { path: rule, methods: null as string[] | null, pass: 0 }
			: { path: rule.path, methods: rule.methods ?? null, pass: rule.passFirst ?? 0 }
	);
	const pageErrors: string[] = [];
	const navigations: string[] = [];
	page.on('pageerror', (error) => pageErrors.push(error.message));
	page.on('framenavigated', (frame) => {
		if (frame === page.mainFrame()) navigations.push(frame.url());
	});
	await page.route('**/v1/**', async (route) => {
		await route.fulfill({
			status: 500,
			json: { code: 'unexpected_fixture_request', message: new URL(route.request().url()).pathname }
		});
	});
	await page.addInitScript(({ rules }) => {
		const nativeFetch = window.fetch.bind(window);
		const pending: Array<{ path: string; settle: (response: Response) => void }> = [];
		const probe: TransitionProbe = {
			requests: [],
			rejections: [],
			writes: [],
			broadcasts: [],
			release: () => {
				throw new Error('No held request');
			}
		};
		window.__transition = probe;
		window.addEventListener('unhandledrejection', (event) =>
			probe.rejections.push(String(event.reason))
		);
		window.fetch = (input, init) => {
			const request = new Request(input, init);
			const pathname = new URL(request.url).pathname;
			const rule = (rules as Array<{ path: string; methods: string[] | null; pass: number }>).find(
				(candidate) =>
					candidate.path === pathname &&
					(candidate.methods === null || candidate.methods.includes(request.method))
			);
			if (!rule) return nativeFetch(input, init);
			if (rule.pass > 0) {
				rule.pass -= 1;
				return nativeFetch(input, init);
			}
			return new Promise<Response>((resolve) => {
				const park = (body: unknown) => {
					probe.requests.push({ method: request.method, path: pathname, body });
					pending.push({
						path: pathname,
						settle: (response) => resolve(response)
					});
				};
				if (request.method === 'GET' || request.method === 'HEAD') park(null);
				else void request.clone().json().then(park, () => park(null));
			});
		};
		probe.release = (path, status, responseBody) => {
			const index = pending.findIndex((item) => item.path === path);
			if (index < 0) throw new Error(`No held request for ${path}`);
			const [item] = pending.splice(index, 1);
			item.settle(
				new Response(JSON.stringify(responseBody), {
					status,
					headers: { 'content-type': 'application/json' }
				})
			);
		};
		for (const method of ['setItem', 'removeItem', 'clear'] as const) {
			const original = Storage.prototype[method];
			// Preserve the real storage side effects; observe writes, not mocks.
			Object.defineProperty(Storage.prototype, method, {
				configurable: true,
				value: function (this: Storage, ...args: string[]) {
					probe.writes.push(`${method}:${args[0] ?? ''}`);
					return Reflect.apply(original, this, args);
				}
			});
		}
		const broadcast = BroadcastChannel.prototype.postMessage;
		BroadcastChannel.prototype.postMessage = function (message) {
			probe.broadcasts.push(this.name);
			return broadcast.call(this, message);
		};
	}, { rules: normalized });
	return { pageErrors, navigations };
}

async function seedState(page: Page) {
	await page.evaluate(
		({ snapshot, context, email }) => {
			for (const storage of [sessionStorage, localStorage]) {
				storage.setItem(context, 'transition-account');
				storage.setItem(
					`${snapshot}.transition-account`,
					JSON.stringify({
						savedAt: '2026-10-19T12:00:00Z',
						items: [{ id: 'held-track', title: 'Keep this snapshot' }]
					})
				);
				storage.setItem(snapshot, 'legacy snapshot fixture');
			}
			sessionStorage.setItem(email, 'fixture@example.com');
		},
		{ snapshot: SNAPSHOT, context: AUTH_CONTEXT, email: EMAIL }
	);
}

async function storedState(page: Page) {
	return page.evaluate(
		({ snapshot, context, email }) => ({
			areas: [sessionStorage, localStorage].map((storage) => ({
				context: storage.getItem(context),
				scoped: storage.getItem(`${snapshot}.transition-account`),
				legacy: storage.getItem(snapshot)
			})),
			email: sessionStorage.getItem(email)
		}),
		{ snapshot: SNAPSHOT, context: AUTH_CONTEXT, email: EMAIL }
	);
}

async function readEmail(page: Page) {
	return page.evaluate((key) => sessionStorage.getItem(key), EMAIL);
}

async function requestCount(page: Page, path: string) {
	return page.evaluate(
		(wanted) => window.__transition.requests.filter((item) => item.path === wanted).length,
		path
	);
}

async function beginSecurity(page: Page, kind: 'logout' | 'revoke-all') {
	const endpoint = kind === 'logout' ? '/v1/session/logout' : '/v1/session/revoke-all';
	const button =
		kind === 'logout' ? /^Sign out$/i : /^Sign out everywhere$/i;
	await page.goto('/account/security');
	const control = page.getByRole('button', { name: button });
	await expect(control).toBeEnabled();
	await seedState(page);
	// Accessible names change to Signing out…/Signing out everywhere… while
	// busy, so pin the element before clicking instead of re-resolving it.
	const element = await control.elementHandle();
	if (!element) throw new Error(`Missing security button: ${kind}`);
	if (kind === 'revoke-all') page.once('dialog', (dialog) => dialog.accept());
	await control.click();
	await expect
		.poll(() => page.evaluate(() => window.__transition.requests))
		.toEqual([{ method: 'POST', path: endpoint, body: {} }]);
	await expect.poll(() => element.evaluate((node) => (node as HTMLButtonElement).disabled)).toBe(true);
	await element.dispose();
	return endpoint;
}

/** A real delegated anchor click, not page.goto/history-only URL replacement. */
async function clientNavigate(page: Page, path: string, requireHeld = true) {
	await page.evaluate((href) => {
		const link = document.createElement('a');
		link.href = href;
		link.textContent = 'Leave held action';
		link.dataset.testid = 'leave-held-action';
		document.body.append(link);
	}, path);
	await page.getByTestId('leave-held-action').click();
	await expect(page).toHaveURL((url) => url.pathname + url.search === path);
	await page.getByTestId('leave-held-action').evaluate((link) => link.remove());
	// This exact probe lives only in the original JS realm. A full reload
	// would lose the pending request and make the negative test meaningless.
	if (requireHeld) expect(await page.evaluate(() => window.__transition.requests.length)).toBeGreaterThan(0);
}

async function release(page: Page, path: string, status: number, body: unknown) {
	await page.evaluate(
		({ path, status, body }) => {
			window.__transition.release(path, status, body);
		},
		{ path, status, body }
	);
}

/** A second tab in the same context performs a real live logout. */
async function actorLogout(page: Page) {
	const actor = await page.context().newPage();
	await actor.route('**/v1/me/subscription', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({
				state: 'no_subscription',
				planId: null,
				status: null,
				currentPeriodEnd: null,
				graceUntil: null,
				cancelAtPeriodEnd: false
			})
		})
	);
	await actor.route('**/v1/session/logout', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({ ok: true })
		})
	);
	await actor.goto('/account/security');
	await actor.getByRole('button', { name: /^sign out$/i }).click();
	await actor.waitForURL('**/login');
	await actor.close();
}

async function breakStorage(page: Page, methods: string[]) {
	await page.evaluate((names) => {
		for (const name of names as string[]) {
			Object.defineProperty(Storage.prototype, name, {
				configurable: true,
				value: function () {
					throw new DOMException('fixture storage denied', 'SecurityError');
				}
			});
		}
	}, methods);
}

async function probeSnapshot(page: Page) {
	return page.evaluate(() => ({
		writes: window.__transition.writes,
		broadcasts: window.__transition.broadcasts,
		rejections: window.__transition.rejections,
		requests: window.__transition.requests
	}));
}

const playbackFixture = { id: 'account-coral', title: 'Account Coral', artist: 'Fixture Artist', streamUrl: '/api/demo/audio?track=coral' };
const servePlaybackLibrary = (page: Page, multi = false) => page.route('**/api/library', (route) =>
	route.fulfill({ json: multi ? [playbackFixture, { id: 'moss', title: 'Moss' }] : [playbackFixture] }));

async function startNativePlayback(page: Page) {
	await page.getByRole('button', { name: 'Play displayed from the top' }).click();
	const player = page.locator(`audio[data-testid="audio-player"][controls=""][src="${playbackFixture.streamUrl}"]`);
	await expect(player).toBeVisible();
	const audio = (await player.elementHandle())!;
	await expect.poll(() => audio.evaluate((node: HTMLAudioElement) =>
		!node.paused && node.seekable.length > 0 && node.currentTime > 0.2)).toBe(true);
	return audio;
}

async function openHome(page: Page) {
	await page.getByTestId('shell-rail').getByRole('link', { name: 'Home', exact: true }).click();
	await expect(page).toHaveURL(/\/home$/);
	await expect(page.getByRole('heading', { name: 'Home', exact: true })).toBeVisible();
	await expect(page.getByTestId('library-list')).toHaveCount(0);
}

test('keyboard Play and Pause keep the range-capable native decoder and position across app routes', async ({ page }) => {
	await servePlaybackLibrary(page);
	await page.goto(`/library?track=${playbackFixture.id}&t=2`);
	const player = page.getByRole('region', { name: 'Player', exact: true });
	const status = player.getByRole('status').and(player.getByTestId('player-transport-status'));
	const play = player.getByRole('button', { name: 'Play', exact: true });
	const pause = player.getByRole('button', { name: 'Pause', exact: true });
	const native = player.locator('audio[controls][data-testid="audio-player"]');
	await expect(native).toBeVisible();
	await expect(status).toHaveAttribute('aria-atomic', 'true');
	await expect(status).toHaveText('Paused');
	await expect(play).toHaveText('Play');
	const audio = (await native.elementHandle())!;
	const time = () => audio.evaluate((node: HTMLAudioElement) => node.currentTime);
	await expect.poll(time).toBeCloseTo(2);
	await audio.evaluate((node: HTMLAudioElement) => {
		const play = node.play;
		// The queued play event can arrive after the media clock has advanced.
		node.play = function () {
			node.dataset.starts = String(Number(node.dataset.starts ?? '0') + 1);
			node.dataset.startedAt = String(node.currentTime);
			return play.call(this);
		};
	});
	await play.press('Enter');
	await expect(status).toHaveText('Playing');
	await expect(pause).toHaveText('Pause');
	await expect(pause).toBeFocused();
	await expect.poll(time).toBeGreaterThan(2.1);
	await pause.press('Space');
	await expect(status).toHaveText('Paused');
	const pausedAt = await time();
	await openHome(page);
	await page.getByRole('link', { name: 'Open Your Library', exact: true }).click();
	await expect(status).toHaveText('Paused');
	await expect(native).toHaveAttribute('data-starts', '1');
	expect(await time()).toBeCloseTo(pausedAt, 3);
	await play.press('Space');
	await expect(status).toHaveText('Playing');
	expect(Number(await native.getAttribute('data-started-at'))).toBeCloseTo(pausedAt, 3);
	await expect.poll(time).toBeGreaterThan(pausedAt + 0.1);
	await audio.evaluate((node: HTMLAudioElement) => node.pause());
	await expect(status).toHaveText('Paused');
	expect(await audio.evaluate((node: HTMLAudioElement) =>
		node === document.querySelector('audio') && node.controls && node.seekable.length > 0 && node.paused)).toBe(true);
	await expect(page.locator('audio')).toHaveCount(1);
});

test('a terminal native resource error disables compact Play but keeps native controls', async ({ page }) => {
	await servePlaybackLibrary(page);
	await page.route('**/api/demo/audio?track=coral', (route) => route.fulfill({ status: 404, body: '' }));
	await page.goto(`/library?track=${playbackFixture.id}&t=2`);
	const player = page.getByRole('region', { name: 'Player', exact: true });
	const status = player.getByRole('status').and(player.getByTestId('player-transport-status'));
	await expect(status).toHaveText('This audio could not be loaded or played. Choose another track.');
	await expect(player.getByRole('button', { name: 'Play', exact: true })).toBeDisabled();
	const native = player.locator('audio[controls][data-testid="audio-player"]');
	await expect(native).toBeVisible();
	expect(await native.evaluate((node: HTMLAudioElement) => node.error !== null)).toBe(true);
});

for (const mode of ['playing', 'demo', 'encoded demo', 'paused', '404 clear', '404 remove', '503 clear', 'timestamp Back']) {
	const timestamp = mode === 'timestamp Back', status = Number.parseInt(mode, 10);
	const paused = timestamp || mode === 'paused', demo = mode.endsWith('demo');
	test(`Library → Home → Library keeps the native decoder (${mode})`, async ({ page }) => {
		const errors: string[] = [];
		page.on('pageerror', (error) => errors.push(error.message));
		await servePlaybackLibrary(page, mode === '404 clear');
		const share = `/library?track=${playbackFixture.id}&t=2`;
		const time = () => page.getByTestId('audio-player').evaluate((node: HTMLAudioElement) => node.currentTime);
		await page.goto(timestamp ? share : '/library');
		if (timestamp) await expect.poll(time).toBeCloseTo(2);
		const audio = await startNativePlayback(page);
		const before = await audio.evaluate((node: HTMLAudioElement, { paused, timestamp }) => {
			if (paused) node.pause();
			if (timestamp) node.currentTime = 4;
			const play = node.play.bind(node);
			node.play = () => { node.dataset.replay = 'called'; return play(); };
			node.addEventListener('play', () => { node.dataset.replay = 'event'; });
			node.addEventListener('loadstart', () => { node.dataset.reload = 'event'; });
			return { position: node.currentTime, source: node.currentSrc };
		}, { paused, timestamp });
		const inspect = async (position: number) => {
			await expect(page.getByTestId('audio-player')).toBeVisible();
			await expect(page.getByTestId('library-now-playing')).toHaveText('Account Coral — Fixture Artist');
			await expect.poll(() => audio.evaluate((node: HTMLAudioElement) => ({
				count: document.querySelectorAll('audio').length, sameNode: node === document.querySelector('audio'),
				source: node.currentSrc, paused: node.paused, replay: node.dataset.replay, reload: node.dataset.reload
			}))).toEqual({ count: 1, sameNode: true, source: before.source, paused, replay: undefined, reload: undefined });
			if (paused) expect(await time()).toBeCloseTo(position, 3);
			else await expect.poll(time).toBeGreaterThan(position + 0.15);
			return time();
		};
		const queueItem = page.getByTestId('queue-item');
		if (status) await queueItem.getByRole('button', { name: `Star ${playbackFixture.title}` }).click();
		await openHome(page);
		const atHome = await inspect(before.position);
		if (status) await page.route('**/api/library', (route) => route.fulfill({ status }), { times: 1 });
		await (timestamp ? page.goBack() : page.getByRole('link', { name: 'Open Your Library', exact: true }).click());
		await expect(status ? page.getByText(status === 404 ? 'Library listing is not available yet.' : 'Library is not reachable right now.') : page.getByTestId('library-item')).toBeVisible();
		await expect(page.getByTestId('now-playing')).toContainText(playbackFixture.title);
		await expect(page.getByTestId('player-next')).toBeVisible();
		const atLibrary = await inspect(atHome);
		if (timestamp) {
			await openHome(page);
			await clientNavigate(page, share, false);
			await expect.poll(time).toBeCloseTo(2);
		} else if (status) {
			const clear = mode.endsWith('clear'), retry = Promise.withResolvers<void>();
			const filter = page.getByRole('searchbox', { name: 'Filter queue' });
			if (status === 503) {
				await filter.fill('Coral');
				await page.route('**/api/library', (route) => retry.promise.then(() => route.fulfill({ status })), { times: 1 });
				await page.getByRole('button', { name: 'Retry' }).click();
				await inspect(atLibrary);
			}
			await expect(queueItem).toHaveCount(mode === '404 clear' ? 2 : 1);
			await expect(queueItem.getByRole('button', { name: /^(Unstar|Star) / })).toHaveCount(0);
			await expect(queueItem.getByRole('checkbox')).toHaveCount(0);
			if (clear) await page.getByRole('button', { name: 'Clear queue', exact: true }).press('Enter');
			await page.getByRole('button', { name: clear ? 'Yes, clear queue' : 'Remove Account Coral from queue' }).press('Enter');
			const emptyFocus = page.getByTestId('queue-empty-focus');
			await expect(emptyFocus).toBeFocused();
			await expect(page.locator('audio')).toHaveCount(0);
			if (status === 503) {
				await expect(page.getByText('Loading your library…')).toBeVisible();
				retry.resolve();
				await expect(page.getByText('Library is not reachable right now.')).toBeVisible();
				await expect(filter).toHaveValue('Coral');
				await expect(emptyFocus).toBeFocused();
			}
			await page.reload();
			await expect(page.getByTestId('favorite-item')).toContainText(playbackFixture.title);
		} else if (!paused) {
			await clientNavigate(page, mode === 'encoded demo' ? '/d%65mo/player' : demo ? '/demo/player' : '/', false);
			await expect(page.getByRole('heading', { name: demo ? 'Player resilience demo' : 'NNTune', exact: true })).toBeVisible();
			if (demo) await expect(page.getByTestId('demo-audio')).toHaveCount(1);
			await expect(page.locator('audio')).toHaveCount(demo ? 1 : 0);
			await expect(page.getByText(playbackFixture.title).or(page.getByTestId('library-now-playing'))).toHaveCount(0);
			expect(await audio.evaluate((node: HTMLAudioElement) => !node.isConnected && node.paused)).toBe(true);
		}
		expect(errors).toEqual([]);
	});
}

for (const signal of ['local', 'cross-tab', 401, 403, 'Billing'] as const) {
	const trigger = typeof signal === 'number' ? `Library HTTP ${signal}` : `${signal} sign-out`;
	test(`${trigger} purges the native player while Library is absent`, async ({ page }) => {
		await servePlaybackLibrary(page);
		const endpoint = '/v1/session/logout';
		if (signal === 'local') {
			await fixtures(page, [endpoint]);
			await beginSecurity(page, 'logout');
			await clientNavigate(page, '/library');
		} else await page.goto('/library');
		const audio = await startNativePlayback(page);
		if (typeof signal === 'number') await page.route('**/api/library', (route) => route.fulfill({ status: signal }), { times: 1 });
		if (signal === 'Billing') {
			await page.route(/\/v1\/me\/(subscription|usage)$/, (route) => route.fulfill({ status: 401, json: { error: 'unauthorized' } }));
			await clientNavigate(page, '/account/billing', false);
			await expect(page.getByRole('alert')).toContainText('You are signed out');
		} else await openHome(page);
		if (typeof signal === 'number') {
			await expect(page.getByText(signal === 401 ? 'Sign in to see your library.' : 'You do not have access to this library.')).toBeVisible();
		} else if (signal !== 'Billing') {
			await expect(page.getByTestId('library-now-playing')).toContainText(playbackFixture.title);
			if (signal === 'local') await release(page, endpoint, 200, { ok: true });
			else await actorLogout(page);
		}
		await expect(page).toHaveURL(signal === 'Billing' ? /\/account\/billing$/ : /\/home$/);
		await expect(page.locator('audio, [data-testid="library-now-playing"], [data-testid="mini-player"]')).toHaveCount(0);
		expect(await audio.evaluate((node: HTMLAudioElement) => !node.isConnected && node.paused)).toBe(true);
		await page.getByTestId('shell-rail').getByRole('link', { name: 'Your Library', exact: true }).click();
		await expect(page.getByTestId('library-item')).toHaveCount(1);
		await expect(page.getByTestId('now-playing')).toHaveText('Queue is empty');
		await expect(page.getByTestId('player-next')).toBeDisabled();
		await expect(page.locator('audio')).toHaveCount(0);
	});
}

test('abandoned logout success resets the same-window replacement library and fences its late response', async ({
	page
}) => {
	const observed = await fixtures(page, ['/v1/session/logout', '/api/library']);
	const endpoint = await beginSecurity(page, 'logout');
	await clientNavigate(page, '/library');
	await expect(page.getByText('Loading your library…')).toBeVisible();
	await expect.poll(() => requestCount(page, '/api/library')).toBe(1);
	observed.navigations.length = 0;
	await page.evaluate(() => {
		window.__transition.writes.length = 0;
		window.__transition.broadcasts.length = 0;
	});

	await release(page, endpoint, 200, { ok: true });

	// The purge landed and the mounted replacement reset to sign-in-required.
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByRole('link', { name: 'Sign in' })).toHaveAttribute('href', '/login');
	await expect(page.getByTestId('library-list')).toHaveCount(0);
	expect(await storedState(page)).toEqual({
		areas: [
			{ context: null, scoped: null, legacy: null },
			{ context: null, scoped: null, legacy: null }
		],
		email: null
	});
	expect.soft(observed.navigations, 'no event-driven navigation').toEqual([]);
	expect.soft(new URL(page.url()).pathname).toBe('/library');
	expect(await requestCount(page, endpoint)).toBe(1);

	// The library response that was already in flight cannot repopulate the
	// reset view, retry the fetch, or rebroadcast the transition.
	await release(page, '/api/library', 200, [{ id: 'late-track', title: 'Late arrival' }]);
	await page.waitForTimeout(350);
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('library-list')).toHaveCount(0);
	expect(await requestCount(page, '/api/library')).toBe(1);
	const probe = await probeSnapshot(page);
	expect(probe.writes.filter((entry) => entry === `setItem:${TRANSITION}`)).toHaveLength(1);
	expect.soft(probe.broadcasts, 'no BroadcastChannel relay').toEqual([]);
	expect.soft(probe.rejections).toEqual([]);
	expect.soft(observed.pageErrors).toEqual([]);
	expect.soft(observed.navigations, 'no late navigation').toEqual([]);
});

test('abandoned revoke-all success resets the same-window replacement library and fences its late response', async ({
	page
}) => {
	const observed = await fixtures(page, ['/v1/session/revoke-all', '/api/library']);
	const endpoint = await beginSecurity(page, 'revoke-all');
	await clientNavigate(page, '/library');
	await expect(page.getByText('Loading your library…')).toBeVisible();
	await expect.poll(() => requestCount(page, '/api/library')).toBe(1);
	observed.navigations.length = 0;
	await page.evaluate(() => {
		window.__transition.writes.length = 0;
		window.__transition.broadcasts.length = 0;
	});

	await release(page, endpoint, 200, { ok: true });

	// The purge landed and the mounted replacement reset to sign-in-required.
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByRole('link', { name: 'Sign in' })).toHaveAttribute('href', '/login');
	await expect(page.getByTestId('library-list')).toHaveCount(0);
	expect(await storedState(page)).toEqual({
		areas: [
			{ context: null, scoped: null, legacy: null },
			{ context: null, scoped: null, legacy: null }
		],
		email: null
	});
	expect.soft(observed.navigations, 'no event-driven navigation').toEqual([]);
	expect.soft(new URL(page.url()).pathname).toBe('/library');
	expect(await requestCount(page, endpoint)).toBe(1);

	// The library response that was already in flight cannot repopulate the
	// reset view, retry the fetch, or rebroadcast the transition.
	await release(page, '/api/library', 200, [{ id: 'late-track', title: 'Late arrival' }]);
	await page.waitForTimeout(350);
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('library-list')).toHaveCount(0);
	expect(await requestCount(page, '/api/library')).toBe(1);
	const probe = await probeSnapshot(page);
	expect(probe.writes.filter((entry) => entry === `setItem:${TRANSITION}`)).toHaveLength(1);
	expect.soft(probe.broadcasts, 'no BroadcastChannel relay').toEqual([]);
	expect.soft(probe.rejections).toEqual([]);
	expect.soft(observed.pageErrors).toEqual([]);
	expect.soft(observed.navigations, 'no late navigation').toEqual([]);
});

test('abandoned revoke-all 401 purges and resets without any message or redirect', async ({
	page
}) => {
	const observed = await fixtures(page, ['/v1/session/revoke-all', '/api/library']);
	const endpoint = await beginSecurity(page, 'revoke-all');
	await clientNavigate(page, '/library');
	await expect(page.getByText('Loading your library…')).toBeVisible();
	await expect.poll(() => requestCount(page, '/api/library')).toBe(1);
	observed.navigations.length = 0;

	// A typed 401 is a qualifying result: conservative purge plus reset, but
	// the abandoned route renders no expired-session message and redirects
	// nowhere. It is not proof of global revocation either.
	await release(page, endpoint, 401, { code: 'unauthorized', message: 'held fixture failure' });
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('library-list')).toHaveCount(0);
	expect(await storedState(page)).toEqual({
		areas: [
			{ context: null, scoped: null, legacy: null },
			{ context: null, scoped: null, legacy: null }
		],
		email: null
	});
	await expect(page.getByText(/session expired before other devices/i)).toHaveCount(0);
	expect.soft(observed.navigations, 'no event-driven navigation').toEqual([]);
	expect.soft(new URL(page.url()).pathname).toBe('/library');
	expect(await requestCount(page, endpoint)).toBe(1);

	await release(page, '/api/library', 200, [{ id: 'late-track', title: 'Late arrival' }]);
	await page.waitForTimeout(350);
	await expect(page.getByTestId('library-list')).toHaveCount(0);
	expect(await requestCount(page, '/api/library')).toBe(1);
	const probe = await probeSnapshot(page);
	expect.soft(probe.rejections).toEqual([]);
	expect.soft(observed.pageErrors).toEqual([]);
});

test('abandoned security failure leaves the mounted replacement library untouched', async ({
	page
}) => {
	const observed = await fixtures(page, ['/v1/session/logout']);
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify([{ id: 'fresh-track', title: 'Fresh Song' }])
		})
	);
	const endpoint = await beginSecurity(page, 'logout');
	await clientNavigate(page, '/library');
	await expect(page.getByTestId('library-list')).toContainText('Fresh Song');
	const before = await storedState(page);
	observed.navigations.length = 0;

	// A non-qualifying failure notifies nothing: the mounted replacement keeps
	// its list, storage is preserved, and no error UI appears.
	await release(page, endpoint, 500, { code: 'internal_error', message: 'held fixture failure' });
	await page.waitForTimeout(350);
	await expect(page.getByTestId('library-list')).toContainText('Fresh Song');
	await expect(page.getByRole('alert')).toHaveCount(0);
	expect(await storedState(page)).toEqual(before);
	expect.soft(observed.navigations).toEqual([]);
	expect.soft(new URL(page.url()).pathname).toBe('/library');
	expect(await requestCount(page, endpoint)).toBe(1);
	const probe = await probeSnapshot(page);
	expect.soft(probe.rejections).toEqual([]);
	expect.soft(observed.pageErrors).toEqual([]);
});

test('abandoned logout in one tab invalidates an open library tab without navigating either', async ({
	page
}) => {
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify([{ id: 'seed-track', title: 'Seeded Song' }])
		})
	);
	await page.addInitScript((key) => {
		sessionStorage.setItem(key, 'library-tab@example.com');
	}, EMAIL);
	const libraryErrors: string[] = [];
	page.on('pageerror', (error) => libraryErrors.push(error.message));
	await page.goto('/library');
	await expect(page.getByTestId('library-list')).toContainText('Seeded Song');

	const securityTab = await page.context().newPage();
	const observed = await fixtures(securityTab, ['/v1/session/logout']);
	const endpoint = await beginSecurity(securityTab, 'logout');
	await clientNavigate(securityTab, '/');
	observed.navigations.length = 0;

	await release(securityTab, endpoint, 200, { ok: true });

	// The delivered transition clears the library tab's own snapshot, memory
	// and per-tab label; the acting tab purged but never navigated.
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('library-list')).toHaveCount(0);
	expect(await readEmail(page)).toBeNull();
	expect.soft(new URL(page.url()).pathname, 'receiver never navigates').toBe('/library');
	expect.soft(new URL(securityTab.url()).pathname, 'abandoned actor never navigates').toBe('/');
	await expect(securityTab.getByRole('alert')).toHaveCount(0);
	expect.soft(observed.navigations).toEqual([]);
	expect(await requestCount(securityTab, endpoint)).toBe(1);
	expect(await storedState(securityTab)).toEqual({
		areas: [
			{ context: null, scoped: null, legacy: null },
			{ context: null, scoped: null, legacy: null }
		],
		email: null
	});
	const probe = await probeSnapshot(securityTab);
	expect.soft(probe.rejections).toEqual([]);
	expect.soft(observed.pageErrors).toEqual([]);
	expect.soft(libraryErrors).toEqual([]);
	await securityTab.close();
});

test('a sign-in that lands before the delayed transition is intentionally expired', async ({
	page
}) => {
	let libraryPosts = 0;
	await page.route('**/api/library', async (route) => {
		libraryPosts += 1;
		await route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify([{ id: 'new-track', title: 'New Session Song' }])
		});
	});
	await page.addInitScript((key) => {
		sessionStorage.setItem(key, 'new-session@example.com');
	}, EMAIL);
	await page.goto('/library');
	await expect(page.getByTestId('library-list')).toContainText('New Session Song');
	expect(await readEmail(page)).toBe('new-session@example.com');

	// The sign-in above committed newer client-visible state; the delayed
	// transition deliberately expires it rather than preserving newer UI.
	await actorLogout(page);

	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('library-list')).toHaveCount(0);
	expect(await readEmail(page)).toBeNull();
	expect(libraryPosts).toBe(1);
	expect.soft(new URL(page.url()).pathname).toBe('/library');
});

test('after the transition, explicit revalidation resumes fresh work while old work stays fenced', async ({
	page
}) => {
	const observed = await fixtures(page, ['/v1/session/logout', '/api/library']);
	const endpoint = await beginSecurity(page, 'logout');
	await clientNavigate(page, '/library');
	await expect(page.getByText('Loading your library…')).toBeVisible();
	await expect.poll(() => requestCount(page, '/api/library')).toBe(1);
	await release(page, endpoint, 200, { ok: true });
	await expect(page.getByRole('alert')).toContainText(/session changed/i);

	// The old held response cannot touch the reset view...
	await release(page, '/api/library', 200, [{ id: 'old-track', title: 'Old arrival' }]);
	await page.waitForTimeout(350);
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('library-list')).toHaveCount(0);

	// ...but an explicit Retry starts ordinary fresh work on the same mount.
	await page.getByRole('button', { name: 'Retry' }).click();
	await expect.poll(() => requestCount(page, '/api/library')).toBe(2);
	await release(page, '/api/library', 200, [{ id: 'fresh-track', title: 'Fresh Song' }]);
	await expect(page.getByTestId('library-list')).toContainText('Fresh Song');
	await expect(page.getByRole('alert')).toHaveCount(0);
	expect(await requestCount(page, endpoint)).toBe(1);
	const probe = await probeSnapshot(page);
	expect.soft(probe.rejections).toEqual([]);
	expect.soft(observed.pageErrors).toEqual([]);
});

test('duplicate transitions settle with no requests, rebroadcasts or navigation', async ({
	page
}) => {
	let libraryPosts = 0;
	await page.route('**/api/library', async (route) => {
		libraryPosts += 1;
		await route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify([{ id: 'seed-track', title: 'Seeded Song' }])
		});
	});
	await page.addInitScript(
		({ email, transition }) => {
			sessionStorage.setItem(email, 'duplicate-tab@example.com');
			const seen: string[] = [];
			(window as unknown as { __seenWrites: string[] }).__seenWrites = seen;
			for (const method of ['setItem', 'removeItem'] as const) {
				const original = Storage.prototype[method];
				Object.defineProperty(Storage.prototype, method, {
					configurable: true,
					value: function (this: Storage, ...args: string[]) {
						seen.push(`${method}:${args[0] ?? ''}`);
						return Reflect.apply(original, this, args);
					}
				});
			}
			void transition;
		},
		{ email: EMAIL, transition: TRANSITION }
	);
	await page.goto('/library');
	await expect(page.getByTestId('library-list')).toContainText('Seeded Song');

	await actorLogout(page);
	await actorLogout(page);

	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('library-list')).toHaveCount(0);
	expect(await readEmail(page)).toBeNull();
	expect(libraryPosts).toBe(1);
	const seen = await page.evaluate(
		() => (window as unknown as { __seenWrites: string[] }).__seenWrites
	);
	expect(
		seen.filter((entry) => entry === `setItem:${TRANSITION}`),
		'receivers never rebroadcast'
	).toEqual([]);
	expect.soft(new URL(page.url()).pathname).toBe('/library');
});

for (const denied of [['setItem', 'removeItem'], ['getItem']] as const) {
	test(`denied storage (${denied.join('/')}) still resets local consumers without throwing`, async ({
		page
	}) => {
		const observed = await fixtures(page, ['/v1/session/logout', '/api/library']);
		const endpoint = await beginSecurity(page, 'logout');
		// Seed first: the denial below must not break the fixture setup, only
		// the purge/broadcast the release triggers.
		await breakStorage(page, [...denied]);
		await clientNavigate(page, '/library');
		await expect.poll(() => requestCount(page, '/api/library')).toBe(1);
		observed.navigations.length = 0;

		await release(page, endpoint, 200, { ok: true });

		// The local reset is independent of storage success: the mounted
		// replacement expires in memory even though the purge cannot write.
		await expect(page.getByRole('alert')).toContainText(/session changed/i);
		await expect(page.getByTestId('library-list')).toHaveCount(0);
		expect.soft(observed.navigations, 'no event-driven navigation').toEqual([]);
		expect(await requestCount(page, endpoint)).toBe(1);
		const probe = await probeSnapshot(page);
		expect.soft(probe.rejections, 'no unhandled rejection').toEqual([]);
		expect.soft(observed.pageErrors, 'no page error').toEqual([]);
	});
}
