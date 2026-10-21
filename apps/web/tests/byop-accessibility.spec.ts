import { expect, test, type Locator, type Page, type Route } from '@playwright/test';

/**
 * Protected behavioral acceptance for the BYOP keyboard/announcement contract.
 * Network boundaries only are mocked; the rendered wizard and browser focus,
 * constraint validation, and ARIA relationships are real. All credentials below
 * are synthetic. The paired implementation must not edit this file.
 *
 * Catches missing step-focus transfer, global-only errors, stale descriptions,
 * keystroke-driven live updates, and button-label-only pending progress.
 */
const PASSWORD = 'fixture-password-never-announce';
const API_KEY = 'fixture-api-key-never-announce';
const LIVE = '[role="status"], [role="alert"], [aria-live="polite"], [aria-live="assertive"], output';

function deferred() {
	let release!: () => void;
	const promise = new Promise<void>((resolve) => { release = resolve; });
	return { promise, release };
}

async function json(route: Route, status: number, body: unknown) {
	await route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });
}

async function mockNetwork(page: Page, options: {
	testGate?: ReturnType<typeof deferred>;
	saveGate?: ReturnType<typeof deferred>;
	failFirstTest?: boolean;
	failFirstSave?: boolean;
	expireProvider?: boolean;
} = {}) {
	const traffic = { tests: 0, saves: 0 };
	await page.route('**/*', async (route) => {
		const url = new URL(route.request().url());
		if (url.pathname === '/v1/vault/test') {
			traffic.tests += 1;
			await options.testGate?.promise;
			if (options.expireProvider) {
				return json(route, 401, { code: 'provider_auth_expired', message: 'provider credentials expired', retryable: false });
			}
			return json(route, 200, options.failFirstTest && traffic.tests === 1
				? { ok: false, latencyMs: 12 }
				: { ok: true, latencyMs: 12 });
		}
		if (url.pathname === '/v1/vault') {
			traffic.saves += 1;
			await options.saveGate?.promise;
			if (options.failFirstSave && traffic.saves === 1) {
				return json(route, 503, { code: 'unavailable', message: 'fixture unavailable', retryable: true });
			}
			return json(route, 200, { ok: true });
		}
		// Deny every unmocked API and non-local origin. No live backend, provider,
		// passkey, or secret service is contacted, including after /library navigation.
		if (url.pathname.startsWith('/v1/') || !['localhost', '127.0.0.1'].includes(url.hostname)) {
			return route.abort('blockedbyclient');
		}
		return route.continue();
	});
	return traffic;
}

async function provider(page: Page, host = 'news.example.test') {
	await page.goto('/onboarding/byop');
	await page.getByTestId('byop-host').fill(host);
	await page.getByTestId('byop-user').fill('fixture-reader');
	await page.getByTestId('byop-pass').fill(PASSWORD);
}

async function indexerStep(page: Page) {
	await provider(page);
	await page.getByRole('button', { name: 'Continue', exact: true }).click();
	await expect(page.getByLabel(/^Indexer URL/)).toBeVisible();
}

async function testStep(page: Page, withIndexer = false) {
	await indexerStep(page);
	if (withIndexer) {
		await page.getByLabel(/^Indexer URL/).fill('https://index.example.test/api');
		await page.getByLabel(/^Indexer API key/).fill(API_KEY);
	}
	await page.getByRole('button', { name: 'Continue', exact: true }).click();
	await expect(page.getByTestId('byop-test-connection')).toBeVisible();
}

async function tabTo(page: Page, target: Locator) {
	for (let i = 0; i < 30; i += 1) {
		if (await target.evaluate((element) => element === document.activeElement)) return;
		await page.keyboard.press('Tab');
	}
	await expect(target, 'control must be reachable by keyboard without a trap').toBeFocused();
}

async function expectStepFocus(page: Page, name: RegExp, firstInput?: Locator) {
	await expect.poll(async () => {
		const heading = await page.evaluate(() => {
			const active = document.activeElement;
			return active?.matches('h1,h2,h3,h4,h5,h6,[role="heading"]') ? active.textContent : null;
		});
		return (heading !== null && name.test(heading)) ||
			(firstInput ? await firstInput.evaluate((element) => element === document.activeElement) : false);
	}, { message: 'rendered step must focus its own heading or first relevant input' }).toBe(true);
}

async function expectInvalidFocus(page: Page, control: Locator) {
	const id = await control.getAttribute('id');
	expect(id, 'invalid control needs a stable ID').toBeTruthy();
	await expect.poll(async () => control.evaluate((element) => {
		const active = document.activeElement;
		return active === element || !!active?.querySelector(`a[href="#${CSS.escape(element.id)}"]`) ||
			(active instanceof HTMLAnchorElement && active.hash === `#${element.id}`);
	}), { message: 'invalid submit must focus the field or a summary linking to it' }).toBe(true);
	if (!(await control.evaluate((element) => element === document.activeElement))) {
		// Locate by the actual ID rather than prescribe an implementation's naming scheme.
		const summaryLink = page.locator(`a[href="#${id}"]`).first();
		await tabTo(page, summaryLink);
		await page.keyboard.press('Enter');
		await expect(control).toBeFocused();
	}
}

async function errorRelationship(page: Page, control: Locator, text: RegExp) {
	await expect(control).toHaveAttribute('aria-invalid', 'true');
	const id = await control.getAttribute('id');
	expect(id, 'control ID must be nonempty').toBeTruthy();
	const description = (await control.getAttribute('aria-describedby') ?? '').trim().split(/\s+/).filter(Boolean);
	expect(description.length, 'custom errors must be described by stable element IDs').toBeGreaterThan(0);
	const errorIds: string[] = [];
	for (const descriptionId of description) {
		const target = page.locator(`[id=${JSON.stringify(descriptionId)}]`);
		await expect(target, 'every description ID must resolve exactly once').toHaveCount(1);
		if (text.test(await target.innerText())) {
			await expect(target).toBeVisible();
			errorIds.push(descriptionId);
		}
	}
	expect(errorIds.length, 'described text must explain this field error').toBeGreaterThan(0);
	await expect(control).toHaveAccessibleDescription(text);
	return { id, errorIds };
}

async function expectCleared(page: Page, control: Locator, oldErrorIds: string[]) {
	await expect(control).not.toHaveAttribute('aria-invalid', 'true');
	const remaining = (await control.getAttribute('aria-describedby') ?? '').split(/\s+/);
	for (const id of oldErrorIds) {
		expect(remaining, 'corrected field must not retain an error description').not.toContain(id);
		await expect(page.locator(`[id=${JSON.stringify(id)}]`)).not.toBeVisible();
	}
}

async function observeAnnouncements(page: Page) {
	await page.evaluate((selector) => {
		const state = { snapshots: [] as string[], mutations: 0 };
		Object.assign(window, { byopAnnouncementProbe: state });
		const read = () => Array.from(document.querySelectorAll(selector))
			.map((node) => `${node.textContent ?? ''} ${node.getAttribute('aria-label') ?? ''}`).join('\n');
		state.snapshots.push(read());
		new MutationObserver((records) => {
			if (records.some((record) => {
				const target = record.target instanceof Element ? record.target : record.target.parentElement;
				return target?.closest(selector) || Array.from(record.addedNodes).some((node) =>
					node instanceof Element && (node.matches(selector) || node.querySelector(selector)));
			})) {
				state.mutations += 1;
				state.snapshots.push(read());
			}
		}).observe(document.querySelector('main')!, { subtree: true, childList: true, characterData: true, attributes: true });
	}, LIVE);
}

async function announcementState(page: Page) {
	// Flush rendering/MutationObserver delivery, without time-based sleeps.
	return page.evaluate(async () => {
		await new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
		return (window as unknown as { byopAnnouncementProbe: { snapshots: string[]; mutations: number } }).byopAnnouncementProbe;
	});
}

async function expectNoSecretAnnouncements(page: Page) {
	const { snapshots } = await announcementState(page);
	for (const snapshot of snapshots) {
		expect(snapshot).not.toContain(PASSWORD);
		expect(snapshot).not.toContain(API_KEY);
	}
}

function progress(page: Page, name: RegExp) {
	return page.locator(LIVE).filter({ hasText: name }).filter({ hasNot: page.locator('button') }).first();
}

test('normal optional-indexer flow remains test-before-save', async ({ page }) => {
	const traffic = await mockNetwork(page);
	await testStep(page);
	const save = page.getByTestId('byop-save');
	expect(await save.isVisible() && await save.isEnabled()).toBe(false);
	await page.getByTestId('byop-test-connection').click();
	await expect(save).toBeEnabled();
	expect(traffic.tests).toBe(1);
	expect(traffic.saves).toBe(0);
	await save.click();
	await page.waitForURL('**/library');
	expect(traffic.saves).toBe(1);
});

test('keyboard forward and back changes focus the rendered step without trapping Tab', async ({ page }) => {
	await mockNetwork(page);
	await provider(page);
	await tabTo(page, page.getByRole('button', { name: 'Continue', exact: true }));
	await page.keyboard.press('Enter');
	await expectStepFocus(page, /indexer/i, page.getByLabel(/^Indexer URL/));
	await tabTo(page, page.getByRole('button', { name: 'Continue', exact: true }));
	await page.keyboard.press('Enter');
	await expectStepFocus(page, /test/i, page.getByTestId('byop-test-connection'));
	await tabTo(page, page.getByRole('button', { name: 'Back', exact: true }));
	await page.keyboard.press('Enter');
	await expectStepFocus(page, /indexer/i, page.getByLabel(/^Indexer URL/));
	await tabTo(page, page.getByRole('button', { name: 'Back', exact: true }));
	await page.keyboard.press('Enter');
	await expectStepFocus(page, /provider/i, page.getByTestId('byop-host'));
	await tabTo(page, page.getByTestId('byop-pass'));
	await expect(page.getByTestId('byop-pass')).toHaveValue(PASSWORD);
});

test('step identity is announced in a stable region, not on each credential keystroke', async ({ page }) => {
	await mockNetwork(page);
	await provider(page);
	await observeAnnouncements(page);
	const initialRegions = await page.locator(LIVE).elementHandles();
	await page.getByRole('button', { name: 'Continue', exact: true }).click();
	const identity = progress(page, /indexer/i);
	await expect(identity).toBeVisible();
	expect(await identity.evaluate((element, candidates) => candidates.includes(element), initialRegions),
		'step announcements must update an already mounted live region').toBe(true);
	await page.getByLabel(/^Indexer URL/).fill('https://index.example.test/api');
	const key = page.getByLabel(/^Indexer API key/);
	await key.focus();
	const before = await announcementState(page);
	await key.pressSequentially(API_KEY);
	await expect(key).toBeFocused();
	const after = await announcementState(page);
	expect(after.mutations, 'typing must not churn live regions').toBe(before.mutations);
	await page.getByRole('button', { name: 'Continue', exact: true }).click();
	await expect(progress(page, /test/i)).toBeVisible();
	await page.getByRole('button', { name: 'Back', exact: true }).click();
	await expect(progress(page, /indexer/i)).toBeVisible();
	await expectNoSecretAnnouncements(page);
});

test('hostname errors focus a reachable control and clear stable associations after correction', async ({ page }) => {
	const traffic = await mockNetwork(page);
	await provider(page, 'https://news.example.test/');
	await page.getByRole('button', { name: 'Continue', exact: true }).click();
	const host = page.getByTestId('byop-host');
	const first = await errorRelationship(page, host, /hostname.*(?:scheme|path)/i);
	await expectInvalidFocus(page, host);
	await host.fill('news.example.test');
	await page.getByRole('button', { name: 'Continue', exact: true }).click();
	await page.getByRole('button', { name: 'Back', exact: true }).click();
	await expectCleared(page, host, first.errorIds);
	await expect(host).toHaveAttribute('id', first.id!);
	await host.fill('https://news.example.test/');
	await page.getByRole('button', { name: 'Continue', exact: true }).click();
	const again = await errorRelationship(page, host, /hostname.*(?:scheme|path)/i);
	expect(again).toEqual(first);
	await expectInvalidFocus(page, host);
	expect(traffic.tests).toBe(0);
	expect(traffic.saves).toBe(0);
});

for (const field of ['url', 'key'] as const) {
	test(`indexer ${field} errors are linked, keyboard reachable, and removed on correction`, async ({ page }) => {
		const traffic = await mockNetwork(page);
		await indexerStep(page);
		const url = page.getByLabel(/^Indexer URL/);
		const key = page.getByLabel(/^Indexer API key/);
		await url.fill(field === 'url' ? 'http://index.example.test/api' : 'https://index.example.test/api');
		if (field === 'url') await key.fill(API_KEY);
		await page.getByRole('button', { name: 'Continue', exact: true }).click();
		const invalid = field === 'url' ? url : key;
		const relationship = await errorRelationship(page, invalid, field === 'url' ? /https/i : /api key/i);
		await expectInvalidFocus(page, invalid);
		// Tab through both controls rather than substituting programmatic focus.
		await tabTo(page, url);
		await url.fill('https://index.example.test/api');
		await tabTo(page, key);
		await key.fill(API_KEY);
		await tabTo(page, page.getByRole('button', { name: 'Continue', exact: true }));
		await page.keyboard.press('Enter');
		await expect(page.getByTestId('byop-test-connection')).toBeVisible();
		await page.getByRole('button', { name: 'Back', exact: true }).click();
		await expectCleared(page, invalid, relationship.errorIds);
		await expect(invalid).toHaveAttribute('id', relationship.id!);
		expect(traffic.tests).toBe(0);
		expect(traffic.saves).toBe(0);
	});
}

test('native required-field validation still focuses the missing provider credential', async ({ page }) => {
	const traffic = await mockNetwork(page);
	await page.goto('/onboarding/byop');
	const host = page.getByTestId('byop-host');
	const user = page.getByTestId('byop-user');
	const password = page.getByTestId('byop-pass');
	for (const field of [host, user, password]) await expect(field).toHaveAttribute('required', '');
	await host.fill('news.example.test');
	await user.fill('fixture-reader');
	await page.getByRole('button', { name: 'Continue', exact: true }).click();
	await expect(password).toBeFocused();
	expect(await password.evaluate((element: HTMLInputElement) => element.validity.valueMissing)).toBe(true);
	await expect(page.getByLabel(/^Indexer URL/)).toHaveCount(0);
	expect(traffic.tests).toBe(0);
	expect(traffic.saves).toBe(0);
});

test('plaintext TLS acknowledgement is still required before advancing', async ({ page }) => {
	const traffic = await mockNetwork(page);
	await provider(page);
	await page.getByTestId('byop-port').selectOption('119');
	await page.getByTestId('byop-tls').uncheck();
	await page.getByRole('button', { name: 'Continue', exact: true }).click();
	await expect(page.getByLabel(/^Indexer URL/)).toHaveCount(0);
	const acknowledgement = page.getByRole('checkbox', { name: /(?:acknowledge|understand)/i });
	await expect(acknowledgement).toBeVisible();
	await acknowledgement.check();
	await page.getByRole('button', { name: 'Continue', exact: true }).click();
	await expect(page.getByLabel(/^Indexer URL/)).toBeVisible();
	expect(traffic.tests).toBe(0);
	expect(traffic.saves).toBe(0);
});

for (const operation of ['test', 'save'] as const) {
	test(`deferred ${operation} announces progress without secrets and clears busy state for retry`, async ({ page }) => {
		const gate = deferred();
		const traffic = await mockNetwork(page, operation === 'test'
			? { testGate: gate, failFirstTest: true }
			: { saveGate: gate, failFirstSave: true });
		await testStep(page, true);
		const testButton = page.getByTestId('byop-test-connection');
		const saveButton = page.getByTestId('byop-save');
		if (operation === 'save') {
			await testButton.click();
			await expect(saveButton).toBeEnabled();
		}
		await observeAnnouncements(page);
		const idleRegions = await page.locator(LIVE).elementHandles();
		const button = operation === 'test' ? testButton : saveButton;
		await button.click();
		await expect.poll(() => operation === 'test' ? traffic.tests : traffic.saves).toBe(1);
		try {
			const pending = progress(page, operation === 'test' ? /testing|checking|test.*(?:pending|progress)/i : /saving|save.*(?:pending|progress)/i);
			await expect(pending, 'pending progress needs a dedicated live status, not only button text').toBeVisible();
			expect(await pending.evaluate((element, candidates) => candidates.includes(element), idleRegions),
				'pending status must update an already mounted live region').toBe(true);
			await expect(page.locator('[aria-busy="true"]'), 'wizard must expose busy state').not.toHaveCount(0);
			await expect(button).toBeDisabled();
			await expectNoSecretAnnouncements(page);
			const statusNode = await pending.elementHandle();
			gate.release();
			await expect(button).toBeEnabled();
			await expect(page.locator('[aria-busy="true"]')).toHaveCount(0);
			expect(await statusNode!.evaluate((element) => element.isConnected), 'status region must survive settlement').toBe(true);
			await expect.poll(() => statusNode!.textContent()).not.toMatch(/testing|checking|saving/i);
			await expectNoSecretAnnouncements(page);
			await button.click();
			if (operation === 'test') {
				await expect(saveButton).toBeEnabled();
				expect(traffic.tests).toBe(2);
				expect(traffic.saves).toBe(0);
			} else {
				await page.waitForURL('**/library');
				expect(traffic.saves).toBe(2);
			}
		} finally {
			gate.release();
		}
	});
}

test('expired provider credentials keep the existing password reauthentication focus', async ({ page }) => {
	const traffic = await mockNetwork(page, { expireProvider: true });
	await testStep(page);
	await observeAnnouncements(page);
	await page.getByTestId('byop-test-connection').click();
	const password = page.locator('main input[type="password"]');
	await expect(password).toBeFocused();
	await password.fill('');
	const before = await announcementState(page);
	await password.pressSequentially(PASSWORD);
	await expect(password).toBeFocused();
	await expect(password).toHaveValue(PASSWORD);
	expect((await announcementState(page)).mutations).toBe(before.mutations);
	await expectNoSecretAnnouncements(page);
	await page.keyboard.press('Tab');
	await expect(password).not.toBeFocused();
	await page.keyboard.press('Shift+Tab');
	await expect(password).toBeFocused();
	expect(traffic.tests).toBe(1);
	expect(traffic.saves).toBe(0);
});
