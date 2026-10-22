import { expect, test, type Page, type Route } from '@playwright/test';

const configured = { provider_configured: true, indexer_configured: true, updated_at: 42 };
const empty = { provider_configured: false, indexer_configured: false, updated_at: 0 };
const stale = { code: 'reauth_required', message: 'reauth required', retryable: false };
const json = (route: Route, body: unknown, status = 200) =>
	route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

function gate() {
	let release!: () => void;
	const promise = new Promise<void>((resolve) => { release = resolve; });
	return { promise, release };
}

async function expire(page: Page) {
	await page.evaluate(() => window.dispatchEvent(new StorageEvent('storage', {
		key: 'nntune.library.auth-transition.v1', newValue: 'synthetic-logout'
	})));
}

async function passkey(page: Page, dismissal?: string) {
	await page.addInitScript((name) => {
		class FixtureCredential {
			id = 'fixture-credential';
			rawId = new Uint8Array([1]).buffer;
			type = 'public-key';
			authenticatorAttachment = 'platform';
			response = {
				clientDataJSON: new Uint8Array([2]).buffer,
				authenticatorData: new Uint8Array([3]).buffer,
				signature: new Uint8Array([4]).buffer,
				userHandle: null
			};
			getClientExtensionResults() { return {}; }
		}
		Object.defineProperty(window, 'PublicKeyCredential', { value: FixtureCredential });
		Object.defineProperty(navigator, 'credentials', {
			value: { get: async () => {
				sessionStorage.setItem('fixture-ceremonies', String(Number(sessionStorage.getItem('fixture-ceremonies') ?? 0) + 1));
				if (name) throw new DOMException('fixture dismissal', name);
				return new FixtureCredential();
			} }
		});
	}, dismissal);
}

async function needsReauth(page: Page) {
	await page.goto('/account/provider');
	page.once('dialog', (dialog) => dialog.accept());
	await page.getByRole('button', { name: 'Remove saved provider' }).click();
	await expect(page.getByRole('alert')).toContainText('Reauthenticate');
}

test('load-time reauth_required offers proof without a configured claim', async ({ page }) => {
	await page.route('**/v1/vault', (route) => json(route, stale, 401));
	await page.goto('/account/provider');
	await expect(page.getByRole('alert')).toContainText('Reauthenticate');
	await expect(page.getByRole('button', { name: 'Reauthenticate with passkey' })).toBeEnabled();
	await expect(page.getByRole('link', { name: 'Sign in again' })).toHaveAttribute('href', '/login');
	await expect(page.getByRole('button', { name: 'Remove saved provider' })).toHaveCount(0);
	await expect(page.getByRole('status')).not.toContainText('Saved provider configured');
});

test('indexer-only settings can be removed but cannot test a missing provider', async ({ page }) => {
	await page.route('**/v1/vault', (route) => json(route, { ...configured, provider_configured: false }));
	await page.goto('/account/provider');
	await expect(page.getByRole('status')).toContainText('No saved provider is configured');
	await expect(page.getByRole('status')).toContainText('Indexer settings are saved');
	await expect(page.getByRole('button', { name: 'Test saved provider' })).toHaveCount(0);
	await expect(page.getByRole('button', { name: 'Remove saved provider' })).toBeEnabled();
});

test('passkey proof does not replay removal; a fresh confirmation is still required', async ({ page }) => {
	await passkey(page);
	let deletes = 0;
	await page.route('**/v1/vault', (route) => {
		if (route.request().method() !== 'DELETE') return json(route, configured);
		deletes += 1;
		return deletes === 1 ? json(route, stale, 401) : json(route, empty);
	});
	await page.route('**/v1/session/reauthenticate/start', (route) => json(route, { challenge: 'YQ', rp_id: 'localhost' }));
	const proofs: unknown[] = [];
	await page.route('**/v1/session/reauthenticate/finish', (route) => {
		proofs.push(route.request().postDataJSON());
		return json(route, { ok: true });
	});
	await needsReauth(page);
	await page.getByRole('button', { name: 'Reauthenticate with passkey' }).click();
	await expect(page.getByRole('status')).toContainText('Identity verified');
	expect(deletes).toBe(1);
	expect(proofs).toEqual([{ challenge: 'YQ', credential_id: 'fixture-credential', authenticator_data: 'Aw', client_data_json: 'Ag', signature: 'BA' }]);
	page.once('dialog', (dialog) => dialog.dismiss());
	await page.getByRole('button', { name: 'Remove saved provider' }).click();
	expect(deletes).toBe(1);
	page.once('dialog', (dialog) => dialog.accept());
	await page.getByRole('button', { name: 'Remove saved provider' }).click();
	await expect(page.getByRole('status')).toContainText('settings removed');
	expect(deletes).toBe(2);
});

for (const dismissal of ['NotAllowedError', 'AbortError']) {
	test(`passkey ${dismissal} settles without repeating proof or removal`, async ({ page }) => {
		await passkey(page, dismissal);
		let deletes = 0;
		let starts = 0;
		let finishes = 0;
		await page.route('**/v1/vault', (route) => {
			if (route.request().method() !== 'DELETE') return json(route, configured);
			deletes += 1;
			return json(route, stale, 401);
		});
		await page.route('**/v1/session/reauthenticate/start', (route) => {
			starts += 1;
			return json(route, { challenge: 'YQ', rp_id: 'localhost' });
		});
		await page.route('**/v1/session/reauthenticate/finish', (route) => { finishes += 1; return json(route, { ok: true }); });
		await needsReauth(page);
		await page.getByRole('button', { name: 'Reauthenticate with passkey' }).click();
		await expect(page.getByRole('alert')).toContainText('dismissed, timed out or unavailable');
		await expect(page.getByRole('button', { name: 'Remove saved provider' })).toBeEnabled();
		await expect(page.getByRole('button', { name: 'Reauthenticate with passkey' })).toBeEnabled();
		expect({ deletes, starts, finishes }).toEqual({ deletes: 1, starts: 1, finishes: 0 });
	});
}

for (const phase of ['start', 'finish']) {
	for (const status of [200, 401, 403, 429]) {
		test(`reauth ${phase} malformed/error HTTP ${status} never verifies or repeats removal`, async ({ page }) => {
			await passkey(page);
			let deletes = 0;
			let finishes = 0;
			await page.route('**/v1/vault', (route) => {
				if (route.request().method() !== 'DELETE') return json(route, configured);
				deletes += 1;
				return json(route, stale, 401);
			});
			await page.route('**/v1/session/reauthenticate/start', (route) =>
				phase === 'start' ? json(route, {}, status) : json(route, { challenge: 'YQ', rp_id: 'localhost' }));
			await page.route('**/v1/session/reauthenticate/finish', (route) => {
				finishes += 1;
				return json(route, {}, status);
			});
			await needsReauth(page);
			await page.getByRole('button', { name: 'Reauthenticate with passkey' }).click();
			if (status === 401) await expect(page.getByRole('status')).toContainText('session has expired');
			else {
				await expect(page.getByRole('button', { name: 'Remove saved provider' })).toBeEnabled();
				if (status === 429) await expect(page.getByRole('dialog')).toBeVisible();
				else await expect(page.getByRole('alert')).toBeVisible();
			}
			await expect(page.locator('main')).not.toContainText(/Identity verified|settings removed/);
			expect(deletes).toBe(1);
			expect(finishes).toBe(phase === 'finish' ? 1 : 0);
		});
	}
}

for (const operation of ['GET', 'POST', 'DELETE']) {
	test(`logout discards a held ${operation} reply and cannot restore saved status`, async ({ page }) => {
		const held = gate();
		const started = gate();
		await page.route('**/v1/vault', async (route) => {
			if (route.request().method() === operation) {
				started.release();
				await held.promise;
			}
			await json(route, route.request().method() === 'DELETE' ? empty : configured);
		});
		await page.route('**/v1/vault/test', async (route) => {
			started.release();
			await held.promise;
			await json(route, { ok: true });
		});
		await page.goto('/account/provider');
		if (operation === 'POST') await page.getByRole('button', { name: 'Test saved provider' }).click();
		if (operation === 'DELETE') {
			page.once('dialog', (dialog) => dialog.accept());
			await page.getByRole('button', { name: 'Remove saved provider' }).click();
		}
		await started.promise;
		await expire(page);
		held.release();
		await expect(page.getByRole('status')).toContainText('session has expired or changed');
		await expect(page.locator('main')).not.toContainText(/Saved provider configured|test passed|settings removed/);
		await expect(page.getByRole('button', { name: /test saved provider|remove saved provider/i })).toHaveCount(0);
	});
}

test('observing an auth transition does not broadcast it back to other tabs', async ({ page }) => {
	await page.route('**/v1/vault', (route) => json(route, configured));
	await page.goto('/account/provider');
	await expect(page.getByRole('status')).toContainText('Saved provider configured');
	const writes = await page.evaluate(() => {
		let count = 0;
		const original = Storage.prototype.setItem;
		Storage.prototype.setItem = function (key, value) {
			if (key === 'nntune.library.auth-transition.v1') count += 1;
			return original.call(this, key, value);
		};
		window.dispatchEvent(new StorageEvent('storage', { key: 'nntune.library.auth-transition.v1' }));
		Storage.prototype.setItem = original;
		return count;
	});
	expect(writes).toBe(0);
});

test('held removal serializes test and refresh, then keeps last-known state on failure', async ({ page }) => {
	const held = gate();
	const started = gate();
	let deletes = 0;
	await page.route('**/v1/vault', async (route) => {
		if (route.request().method() === 'DELETE') {
			deletes += 1;
			started.release();
			await held.promise;
			await json(route, { error: 'fixture-only' }, 500);
		} else await json(route, configured);
	});
	await page.goto('/account/provider');
	page.once('dialog', (dialog) => dialog.accept());
	await page.getByRole('button', { name: 'Remove saved provider' }).click();
	await started.promise;
	for (const name of ['Test saved provider', 'Remove saved provider', 'Refresh saved status']) {
		await expect(page.getByRole('button', { name })).toBeDisabled();
	}
	held.release();
	await expect(page.getByRole('alert')).toContainText('Could not confirm removal');
	await expect(page.getByRole('button', { name: 'Remove saved provider' })).toBeEnabled();
	expect(deletes).toBe(1);
});

test('navigation during reauth start cannot open a late ceremony or submit proof', async ({ page }) => {
	await passkey(page);
	const held = gate();
	const started = gate();
	let proofs = 0;
	await page.route('**/v1/vault', (route) => route.request().method() === 'DELETE' ? json(route, stale, 401) : json(route, configured));
	await page.route('**/v1/session/reauthenticate/start', async (route) => {
		started.release();
		await held.promise;
		await json(route, { challenge: 'YQ', rp_id: 'localhost' });
	});
	await page.route('**/v1/session/reauthenticate/finish', (route) => { proofs += 1; return json(route, { ok: true }); });
	await needsReauth(page);
	await page.getByRole('button', { name: 'Reauthenticate with passkey' }).click();
	await started.promise;
	await page.getByRole('link', { name: 'Security', exact: true }).click();
	await expect(page.getByRole('heading', { name: 'Account security' })).toBeVisible();
	held.release();
	await page.getByRole('link', { name: 'Provider', exact: true }).click();
	await expect(page.getByRole('status')).toContainText('Saved provider configured');
	expect(proofs).toBe(0);
	expect(await page.evaluate(() => sessionStorage.getItem('fixture-ceremonies'))).toBeNull();
});

test('a same-page hash navigation keeps the saved surface usable', async ({ page }) => {
	await page.route('**/v1/vault', (route) => json(route, configured));
	await page.goto('/account/provider');
	await expect(page.getByRole('status')).toContainText('Saved provider configured');
	await page.evaluate(() => {
		const link = document.createElement('a');
		link.href = '/account/provider#removal-heading';
		link.textContent = 'Jump to removal';
		document.querySelector('main')!.append(link);
	});
	await page.getByRole('link', { name: 'Jump to removal' }).click();
	await expect(page.getByRole('status')).toContainText('Saved provider configured');
	await expect(page.getByRole('button', { name: 'Test saved provider' })).toBeEnabled();
});

test('a back-forward-cache restore refreshes instead of retaining the old account summary', async ({ page }) => {
	let saved = true;
	await page.route('**/v1/vault', (route) => json(route, saved ? configured : empty));
	await page.goto('/account/provider');
	await expect(page.getByRole('status')).toContainText('Saved provider configured');
	await page.evaluate(() => window.dispatchEvent(new PageTransitionEvent('pagehide', { persisted: true })));
	saved = false;
	await page.evaluate(() => window.dispatchEvent(new PageTransitionEvent('pageshow', { persisted: true })));
	await expect(page.getByRole('status')).toContainText('No saved provider is configured');
});

for (const status of [401, 403, 404, 429]) {
	test(`stored retest HTTP ${status} settles with no success or secret disclosure`, async ({ page }) => {
		await page.route('**/v1/vault', (route) => json(route, configured));
		let posts = 0;
		await page.route('**/v1/vault/test', (route) => {
			posts += 1;
			return json(route, { error: 'DO-NOT-RENDER-SYNTHETIC-SECRET' }, status);
		});
		await page.goto('/account/provider');
		await page.getByRole('button', { name: 'Test saved provider' }).click();
		if (status === 401) {
			await expect(page.getByRole('status')).toContainText('session has expired');
		} else {
			await expect(page.getByRole('button', { name: 'Test saved provider' })).toBeEnabled();
			if (status === 429) await expect(page.getByRole('dialog')).toBeVisible();
			else await expect(page.getByRole('alert')).toBeVisible();
		}
		await expect(page.locator('body')).not.toContainText(/test passed|DO-NOT-RENDER-SYNTHETIC-SECRET/);
		expect(posts).toBe(1);
	});
}

for (const viewport of [{ width: 1280, height: 800 }, { width: 390, height: 844 }]) {
	test(`saved-provider visual states at ${viewport.width}px`, async ({ page }, info) => {
		await page.setViewportSize(viewport);
		await page.emulateMedia({ colorScheme: 'light', reducedMotion: 'reduce' });
		let state = 'loading';
		let release = gate();
		await page.route('**/v1/vault', async (route) => {
			if (state === 'loading') await release.promise;
			if (state === 'error') return json(route, {}, 500);
			if (state === 'expired') return json(route, {}, 401);
			if (state === 'reauth' && route.request().method() === 'DELETE') return json(route, stale, 401);
			return json(route, state === 'empty' || route.request().method() === 'DELETE' ? empty : configured);
		});
		await page.route('**/v1/vault/test', (route) => json(route, { ok: state !== 'failed' }));
		async function capture(name: string) {
			expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
			await page.screenshot({ path: info.outputPath(`${viewport.width}-${name}.png`), fullPage: true });
		}
		await page.goto('/account/provider');
		await expect(page.getByRole('status')).toContainText('Loading');
		await capture('loading');
		release.release();
		await expect(page.getByRole('status')).toContainText('Saved provider configured');
		await capture('configured');
		await page.getByRole('button', { name: 'Test saved provider' }).click();
		await expect(page.getByRole('status')).toContainText('test passed');
		await capture('reachable');
		state = 'failed';
		await page.getByRole('button', { name: 'Test saved provider' }).click();
		await expect(page.getByRole('status')).toContainText('test failed');
		await capture('unreachable');
		state = 'reauth';
		page.once('dialog', (dialog) => dialog.accept());
		await page.getByRole('button', { name: 'Remove saved provider' }).click();
		await expect(page.getByRole('button', { name: 'Reauthenticate with passkey' })).toBeVisible();
		await capture('reauth');
		for (const next of ['empty', 'error', 'expired']) {
			state = next;
			await page.reload();
			if (next === 'empty') await expect(page.getByRole('status')).toContainText('No saved provider');
			if (next === 'error') await expect(page.getByRole('alert')).toBeVisible();
			if (next === 'expired') await expect(page.getByRole('status')).toContainText('session has expired');
			await capture(next);
		}
	});
}
