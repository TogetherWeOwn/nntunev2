import { expect, test, type Page } from '@playwright/test';

/**
 * Acceptance: review provider settings and deliberately skip an optional
 * indexer in the BYOP onboarding wizard.
 *
 * Contract under test (case ids are stable for handoff reporting):
 *
 * - The final step shows an accessible, redacted summary of what is about
 *   to be tested/saved: provider hostname, port, TLS on/off, connection
 *   limit, and indexer configured / not configured. Secrets (username,
 *   password, indexer API key, full indexer URL, hashes, lengths) never
 *   appear in the summary, in announcements, or in console output.
 * - "Edit provider" and "Edit indexer" return through the guarded back
 *   transition, moving focus to the step heading and preserving typed
 *   values, validation, and the plaintext-TLS acknowledgement. Editing
 *   voids tested-configuration proof: Save stays unavailable until the
 *   current configuration passes the connection test again.
 * - "Continue without indexer" on the optional indexer step deliberately
 *   clears both indexer fields, voids prior proof, preserves provider
 *   fields, and reaches the final step without any test/save request. A
 *   partial or invalid indexer is never silently sent afterwards.
 * - Existing payload/schema, test-before-save, busy/saved/reset fences,
 *   and connectivity-only wording are unchanged. Review, edit, and skip
 *   navigation make no network calls.
 *
 * PC-* cases are positive controls over already-shipped behavior: they
 * must pass before and after the feature lands. RVS-* cases pin the new
 * behavior. All API responses are synthetic route fixtures and all
 * credentials are nonsecret sentinels; no live provider or backend calls.
 */

const HOST = 'news.example.test';
const NEW_HOST = 'news-backup.example.test';
const SENTINEL_USER = 'sentinel-user-rvs';
const SENTINEL_PASSWORD = 'sentinel-password-rvs';
const SENTINEL_NEW_PASSWORD = 'sentinel-new-password-rvs';
const SENTINEL_INDEXER_URL = 'https://indexer.example.test/sentinel-path-rvs';
const SENTINEL_INDEXER_KEY = 'sentinel-indexer-key-rvs';
const SENTINEL_NEW_INDEXER_KEY = 'sentinel-new-indexer-key-rvs';

const SECRETS = [
	SENTINEL_USER,
	SENTINEL_PASSWORD,
	SENTINEL_NEW_PASSWORD,
	SENTINEL_INDEXER_URL,
	'sentinel-path-rvs',
	'indexer.example.test',
	SENTINEL_INDEXER_KEY,
	SENTINEL_NEW_INDEXER_KEY
] as const;

const PROVIDER_PAYLOAD_KEYS = [
	'allow_insecure_plaintext',
	'host',
	'max_connections',
	'password',
	'port',
	'tls',
	'username'
];

type JsonBody = Record<string, unknown>;

interface Traffic {
	testBodies: JsonBody[];
	saveBodies: JsonBody[];
	reauthPosts: number;
}

/** A promise released from test code, for holding a route open. */
function held(): { promise: Promise<void>; release: () => void } {
	let release!: () => void;
	const promise = new Promise<void>((resolve) => {
		release = resolve;
	});
	return { promise, release };
}

interface VaultRoutes {
	traffic: Traffic;
	releaseTests: () => void;
	releaseSaves: () => void;
	firstTestStarted: Promise<void>;
	firstSaveStarted: Promise<void>;
}

/**
 * Mock the provider test, vault save, and reauthentication boundaries.
 * Tests and saves can be held open so busy-state fences are observable.
 */
async function routeVault(
	page: Page,
	options: { holdTests?: boolean; holdSaves?: boolean } = {}
): Promise<VaultRoutes> {
	const traffic: Traffic = { testBodies: [], saveBodies: [], reauthPosts: 0 };
	const testHold = held();
	const saveHold = held();
	const firstTest = held();
	const firstSave = held();

	await page.route('**/v1/vault/test', async (route) => {
		traffic.testBodies.push(route.request().postDataJSON() as JsonBody);
		if (traffic.testBodies.length === 1) firstTest.release();
		if (options.holdTests) await testHold.promise;
		await route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({ ok: true, latencyMs: 12 })
		});
	});
	await page.route('**/v1/vault', async (route) => {
		traffic.saveBodies.push(route.request().postDataJSON() as JsonBody);
		if (traffic.saveBodies.length === 1) firstSave.release();
		if (options.holdSaves) await saveHold.promise;
		await route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({ ok: true })
		});
	});
	await page.route('**/v1/session/reauthenticate/**', async (route) => {
		traffic.reauthPosts += 1;
		await route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({ challenge: 'Y2hhbGxlbmdl', rp_id: 'localhost', ok: true })
		});
	});

	return {
		traffic,
		releaseTests: testHold.release,
		releaseSaves: saveHold.release,
		firstTestStarted: firstTest.promise,
		firstSaveStarted: firstSave.promise
	};
}

/**
 * The shipped wire envelope nests provider fields under `provider` with
 * an optional sibling `indexer`. Pin the exact key sets so a payload or
 * schema drift fails here instead of slipping through as a subset match.
 */
function expectPayloadKeys(body: JsonBody | undefined, withIndexer: boolean): void {
	expect(body, 'a request body was captured').toBeDefined();
	expect(Object.keys(body ?? {}).sort()).toEqual(
		withIndexer ? ['indexer', 'provider'] : ['provider']
	);
	const provider = (body?.['provider'] ?? {}) as JsonBody;
	expect(Object.keys(provider).sort()).toEqual(PROVIDER_PAYLOAD_KEYS);
	if (withIndexer) {
		const indexer = (body?.['indexer'] ?? {}) as JsonBody;
		expect(Object.keys(indexer).sort()).toEqual(['api_key', 'url']);
	}
}

/**
 * Click a button that is part of the new contract. Visibility is
 * asserted first so a missing control fails as a crisp assertion
 * instead of a long click timeout.
 */
async function clickButton(page: Page, name: RegExp): Promise<void> {
	const button = page.getByRole('button', { name });
	await expect(button).toBeVisible();
	await button.click();
}

function expectNoCalls(traffic: Traffic): void {
	expect(traffic.testBodies, 'no connection-test request').toHaveLength(0);
	expect(traffic.saveBodies, 'no save request').toHaveLength(0);
	expect(traffic.reauthPosts, 'no reauthentication request').toBe(0);
}

/** Collect console output so secret-leak assertions can cover logs. */
function watchConsole(page: Page): string[] {
	const lines: string[] = [];
	page.on('console', (message) => lines.push(message.text()));
	page.on('pageerror', (error) => lines.push(error.message));
	return lines;
}

/**
 * The visible page, its live announcements, and its console output must
 * never carry a sentinel secret or a hash-like fingerprint of one.
 */
async function expectRedacted(page: Page, consoleLines: string[]): Promise<void> {
	const mainText = await page.locator('main').innerText();
	const announcements = await page
		.locator('main [role="status"], main [role="alert"], main [aria-live]')
		.allInnerTexts();
	for (const secret of SECRETS) {
		expect(mainText, 'page text stays redacted').not.toContain(secret);
		for (const announcement of announcements) {
			expect(announcement, 'announcements stay redacted').not.toContain(secret);
		}
	}
	expect(consoleLines.join('\n'), 'console output stays redacted').not.toMatch(
		/sentinel-(user|password|indexer|path|new)/i
	);
	expect(mainText, 'no hash-like credential fingerprint').not.toMatch(/\b[0-9a-f]{32,}\b/i);
}

interface SummaryExpectation {
	host: string;
	port: number;
	tlsOn: boolean;
	connections: number;
	indexerConfigured: boolean;
}

/** Assert the final-step redacted summary shows each pinned field. */
async function expectSummary(page: Page, expected: SummaryExpectation): Promise<void> {
	const section = page.getByRole('region', { name: 'Test the connection' });
	await expect(section).toBeVisible();
	const text = await section.innerText();
	expect(text, 'summary shows provider hostname as text').toContain(expected.host);
	expect(text, 'summary shows port').toMatch(new RegExp(`\\b${expected.port}\\b`));
	expect(text, 'summary names TLS').toMatch(/tls/i);
	if (expected.tlsOn) {
		expect(text, 'summary shows TLS on').toMatch(
			/tls[^.\n]{0,24}\b(on|enabled|yes)\b|\b(on|enabled)\b[^.\n]{0,24}tls/i
		);
		expect(text, 'summary does not claim TLS off').not.toMatch(
			/without tls|no tls|tls[^.\n]{0,16}\b(off|disabled)\b/i
		);
	} else {
		expect(text, 'summary shows TLS off').toMatch(
			/without tls|no tls|tls[^.\n]{0,24}\b(off|disabled)\b|plaintext/i
		);
	}
	expect(text, 'summary shows connection limit').toMatch(
		new RegExp(
			`connection[^.\\n]{0,20}\\b${expected.connections}\\b|\\b${expected.connections}\\b[^.\\n]{0,20}connection`,
			'i'
		)
	);
	if (expected.indexerConfigured) {
		expect(text, 'summary says an indexer is configured').toMatch(
			/indexer[^.\n]{0,32}\b(configured|added|included)\b/i
		);
		expect(text, 'configured summary never says not configured').not.toMatch(
			/indexer[^.\n]{0,24}\bnot (configured|set)\b/i
		);
	} else {
		expect(text, 'summary says no indexer is configured').toMatch(
			/indexer[^.\n]{0,32}\b(not configured|not set|none|skipped)\b|no indexer/i
		);
	}
}

async function fillProvider(
	page: Page,
	options: { host?: string; connections?: number; tlsOff?: boolean } = {}
): Promise<void> {
	await page.goto('/onboarding/byop');
	await page.getByLabel('Hostname').fill(options.host ?? HOST);
	if (options.tlsOff) {
		await page.getByLabel('Port').selectOption('119');
		await page.getByTestId('byop-tls').uncheck();
		await page
			.getByLabel(/I understand my password will be sent unencrypted/)
			.check();
	}
	await page.getByLabel('Username').fill(SENTINEL_USER);
	await page.getByLabel('Password', { exact: true }).fill(SENTINEL_PASSWORD);
	if (options.connections !== undefined) {
		await page.getByLabel('Connections').fill(String(options.connections));
	}
	await page.getByRole('button', { name: 'Continue' }).click();
	await expect(page.getByRole('heading', { name: 'Indexer (optional)' })).toBeFocused();
}

async function fillIndexer(page: Page): Promise<void> {
	await page.getByLabel('Indexer URL').fill(SENTINEL_INDEXER_URL);
	await page.getByLabel('Indexer API key').fill(SENTINEL_INDEXER_KEY);
}

async function continueToFinalStep(page: Page): Promise<void> {
	await page.getByRole('button', { name: 'Continue', exact: true }).click();
	await expect(page.getByRole('heading', { name: 'Test the connection' })).toBeFocused();
	await expect(page.getByTestId('byop-test-connection')).toBeVisible();
}

/** Drive to the final step and earn tested-configuration proof. */
async function reachTestedFinalStep(
	page: Page,
	options: { withIndexer?: boolean } = {}
): Promise<void> {
	await fillProvider(page, { connections: 7 });
	if (options.withIndexer) await fillIndexer(page);
	await continueToFinalStep(page);
	await page.getByTestId('byop-test-connection').click();
	await expect(page.getByTestId('byop-save')).toBeEnabled();
}

test.describe('positive controls (shipped behavior)', () => {
	test('PC-01 existing flow tests then saves the exact unchanged payload', async ({
		page
	}) => {
		const vault = await routeVault(page);
		await reachTestedFinalStep(page, { withIndexer: true });

		expectPayloadKeys(vault.traffic.testBodies[0], true);
		expect(vault.traffic.testBodies[0]).toMatchObject({
			provider: {
				host: HOST,
				port: 563,
				tls: true,
				allow_insecure_plaintext: false,
				username: SENTINEL_USER,
				max_connections: 7
			},
			indexer: { url: SENTINEL_INDEXER_URL, api_key: SENTINEL_INDEXER_KEY }
		});

		await page.getByTestId('byop-save').click();
		await page.waitForURL('/library');
		expect(vault.traffic.saveBodies).toHaveLength(1);
		expect(vault.traffic.saveBodies[0]).toEqual(vault.traffic.testBodies[0]);
	});

	test('PC-02 leaving the final step with Back still voids the save binding', async ({
		page
	}) => {
		const vault = await routeVault(page);
		await reachTestedFinalStep(page);

		await page.getByRole('button', { name: 'Back' }).click();
		await continueToFinalStep(page);
		await expect(page.getByTestId('byop-save')).toHaveCount(0);
		expect(vault.traffic.testBodies).toHaveLength(1);
		expect(vault.traffic.saveBodies).toHaveLength(0);
	});
});

test.describe('redacted review summary', () => {
	test('RVS-01 final step summarizes provider fields and a configured indexer without secrets', async ({
		page
	}) => {
		const consoleLines = watchConsole(page);
		const vault = await routeVault(page);
		await fillProvider(page, { connections: 7 });
		await fillIndexer(page);
		await continueToFinalStep(page);

		await expectSummary(page, {
			host: HOST,
			port: 563,
			tlsOn: true,
			connections: 7,
			indexerConfigured: true
		});
		await expectRedacted(page, consoleLines);

		// The summary is a review aid, not proof of storage or credential
		// validity: connectivity-only wording stays and Save needs a test.
		const section = page.getByRole('region', { name: 'Test the connection' });
		await expect(section).toContainText('without storing anything yet');
		await expect(section).toContainText('does not verify provider credentials');
		await expect(page.getByTestId('byop-save')).toHaveCount(0);
		expectNoCalls(vault.traffic);
	});

	test('RVS-02 summary reports TLS off, port 119 and no indexer configured', async ({
		page
	}) => {
		const consoleLines = watchConsole(page);
		const vault = await routeVault(page);
		await fillProvider(page, { connections: 3, tlsOff: true });
		await continueToFinalStep(page);

		await expectSummary(page, {
			host: HOST,
			port: 119,
			tlsOn: false,
			connections: 3,
			indexerConfigured: false
		});
		await expectRedacted(page, consoleLines);
		expectNoCalls(vault.traffic);
	});
});

test.describe('edit and return', () => {
	test('RVS-03 Edit provider preserves values, then changed settings need a fresh test', async ({
		page
	}) => {
		const vault = await routeVault(page);
		await reachTestedFinalStep(page, { withIndexer: true });

		await clickButton(page, /edit provider/i);
		await expect(page.getByRole('heading', { name: 'Provider details' })).toBeFocused();
		await expect(page.getByText('Step 1 of 3')).toBeVisible();

		// Everything typed in this visit is still there; the edit itself
		// sent nothing.
		await expect(page.getByLabel('Hostname')).toHaveValue(HOST);
		await expect(page.getByLabel('Username')).toHaveValue(SENTINEL_USER);
		await expect(page.getByLabel('Password', { exact: true })).toHaveValue(
			SENTINEL_PASSWORD
		);
		await expect(page.getByLabel('Port')).toHaveValue('563');
		await expect(page.getByTestId('byop-tls')).toBeChecked();
		await expect(page.getByLabel('Connections')).toHaveValue('7');
		expect(vault.traffic.testBodies).toHaveLength(1);
		expect(vault.traffic.saveBodies).toHaveLength(0);

		await page.getByLabel('Hostname').fill(NEW_HOST);
		await page.getByLabel('Connections').fill('9');
		await page.getByRole('button', { name: 'Continue' }).click();
		await expect(page.getByLabel('Indexer URL')).toHaveValue(SENTINEL_INDEXER_URL);
		await expect(page.getByLabel('Indexer API key')).toHaveValue(SENTINEL_INDEXER_KEY);
		await continueToFinalStep(page);

		await expectSummary(page, {
			host: NEW_HOST,
			port: 563,
			tlsOn: true,
			connections: 9,
			indexerConfigured: true
		});
		// The earlier passing test is stale proof for changed settings.
		await expect(page.getByTestId('byop-save')).toHaveCount(0);

		await page.getByTestId('byop-test-connection').click();
		await expect(page.getByTestId('byop-save')).toBeEnabled();
		expect(vault.traffic.testBodies).toHaveLength(2);
		expectPayloadKeys(vault.traffic.testBodies[1], true);
		expect(vault.traffic.testBodies[1]).toMatchObject({
			provider: { host: NEW_HOST, max_connections: 9 }
		});
		await page.getByTestId('byop-save').click();
		await page.waitForURL('/library');
		expect(vault.traffic.saveBodies[0]).toEqual(vault.traffic.testBodies[1]);
	});

	test('RVS-04 Edit provider preserves the plaintext acknowledgement', async ({ page }) => {
		const vault = await routeVault(page);
		await fillProvider(page, { connections: 5, tlsOff: true });
		await continueToFinalStep(page);
		await page.getByTestId('byop-test-connection').click();
		await expect(page.getByTestId('byop-save')).toBeEnabled();

		await clickButton(page, /edit provider/i);
		await expect(page.getByRole('heading', { name: 'Provider details' })).toBeFocused();
		await expect(page.getByTestId('byop-tls')).not.toBeChecked();
		await expect(
			page.getByLabel(/I understand my password will be sent unencrypted/)
		).toBeChecked();
		await expect(page.getByLabel('Port')).toHaveValue('119');

		// Continuing needs no fresh acknowledgement and shows no error.
		await page.getByRole('button', { name: 'Continue' }).click();
		await expect(page.getByRole('heading', { name: 'Indexer (optional)' })).toBeFocused();
		await expect(page.getByRole('alert')).toHaveCount(0);
		expect(vault.traffic.testBodies).toHaveLength(1);
		expect(vault.traffic.saveBodies).toHaveLength(0);
	});

	test('RVS-05 Edit indexer preserves values and only the edited key is retested', async ({
		page
	}) => {
		const consoleLines = watchConsole(page);
		const vault = await routeVault(page);
		await reachTestedFinalStep(page, { withIndexer: true });

		await clickButton(page, /edit indexer/i);
		await expect(page.getByRole('heading', { name: 'Indexer (optional)' })).toBeFocused();
		await expect(page.getByText('Step 2 of 3')).toBeVisible();
		await expect(page.getByLabel('Indexer URL')).toHaveValue(SENTINEL_INDEXER_URL);
		await expect(page.getByLabel('Indexer API key')).toHaveValue(SENTINEL_INDEXER_KEY);
		expect(vault.traffic.testBodies).toHaveLength(1);
		expect(vault.traffic.saveBodies).toHaveLength(0);

		await page.getByLabel('Indexer API key').fill(SENTINEL_NEW_INDEXER_KEY);
		await continueToFinalStep(page);

		// Stale proof from the previous key cannot save the edited config.
		await expect(page.getByTestId('byop-save')).toHaveCount(0);
		await expectRedacted(page, consoleLines);

		await page.getByTestId('byop-test-connection').click();
		await expect(page.getByTestId('byop-save')).toBeEnabled();
		expect(vault.traffic.testBodies).toHaveLength(2);
		expectPayloadKeys(vault.traffic.testBodies[1], true);
		expect(vault.traffic.testBodies[1]).toMatchObject({
			indexer: { url: SENTINEL_INDEXER_URL, api_key: SENTINEL_NEW_INDEXER_KEY }
		});
		expect(JSON.stringify(vault.traffic.testBodies[1])).not.toContain(
			`"${SENTINEL_INDEXER_KEY}"`
		);
	});

	test('RVS-06 editing without changes still voids tested proof', async ({ page }) => {
		const vault = await routeVault(page);
		await reachTestedFinalStep(page, { withIndexer: true });

		await clickButton(page, /edit provider/i);
		await page.getByRole('button', { name: 'Continue' }).click();
		await continueToFinalStep(page);

		await expect(page.getByTestId('byop-save')).toHaveCount(0);
		await page.getByTestId('byop-test-connection').click();
		await expect(page.getByTestId('byop-save')).toBeEnabled();
		expect(vault.traffic.testBodies).toHaveLength(2);
		expect(vault.traffic.testBodies[1]).toEqual(vault.traffic.testBodies[0]);
		expect(vault.traffic.saveBodies).toHaveLength(0);
	});
});

test.describe('deliberate skip', () => {
	test('RVS-07 Continue without indexer clears a partial indexer and keeps the provider', async ({
		page
	}) => {
		const consoleLines = watchConsole(page);
		const vault = await routeVault(page);
		await fillProvider(page, { connections: 7 });

		// Partial indexer: a key with no URL cannot Continue normally.
		await page.getByLabel('Indexer API key').fill(SENTINEL_INDEXER_KEY);
		await clickButton(page, /continue without indexer/i);

		await expect(page.getByRole('heading', { name: 'Test the connection' })).toBeFocused();
		await expect(page.getByRole('alert')).toHaveCount(0);
		expectNoCalls(vault.traffic);
		await expectSummary(page, {
			host: HOST,
			port: 563,
			tlsOn: true,
			connections: 7,
			indexerConfigured: false
		});
		await expectRedacted(page, consoleLines);

		// The cleared fields are really empty back on the indexer step.
		await clickButton(page, /edit indexer/i);
		await expect(page.getByLabel('Indexer URL')).toHaveValue('');
		await expect(page.getByLabel('Indexer API key')).toHaveValue('');
		await continueToFinalStep(page);

		// The next test sends provider fields only: no indexer member and
		// no trace of the abandoned key.
		await page.getByTestId('byop-test-connection').click();
		await expect(page.getByTestId('byop-save')).toBeEnabled();
		expectPayloadKeys(vault.traffic.testBodies[0], false);
		expect(vault.traffic.testBodies[0]).toMatchObject({
			provider: {
				host: HOST,
				port: 563,
				tls: true,
				username: SENTINEL_USER,
				max_connections: 7
			}
		});
		expect(JSON.stringify(vault.traffic.testBodies[0])).not.toContain('sentinel-indexer');
	});

	test('RVS-08 skipping after a tested configuration voids proof and drops an invalid indexer', async ({
		page
	}) => {
		const vault = await routeVault(page);
		await reachTestedFinalStep(page, { withIndexer: true });

		await page.getByRole('button', { name: 'Back' }).click();
		await expect(page.getByRole('heading', { name: 'Indexer (optional)' })).toBeFocused();
		// Replace the tested indexer with one that fails validation, then
		// deliberately skip instead of fixing it.
		await page
			.getByLabel('Indexer URL')
			.fill('http://indexer.example.test/sentinel-path-rvs');
		await clickButton(page, /continue without indexer/i);

		await expect(page.getByRole('heading', { name: 'Test the connection' })).toBeFocused();
		await expect(page.getByRole('alert')).toHaveCount(0);
		// Prior proof did not survive the skip; the skip made no request.
		await expect(page.getByTestId('byop-save')).toHaveCount(0);
		expect(vault.traffic.testBodies).toHaveLength(1);
		expect(vault.traffic.saveBodies).toHaveLength(0);

		await page.getByTestId('byop-test-connection').click();
		await expect(page.getByTestId('byop-save')).toBeEnabled();
		expect(vault.traffic.testBodies).toHaveLength(2);
		expectPayloadKeys(vault.traffic.testBodies[1], false);
	});
});

test.describe('navigation and operation fences', () => {
	test('RVS-09 review, edit and skip navigation make no network calls', async ({
		page
	}) => {
		const vault = await routeVault(page);
		await fillProvider(page, { connections: 7 });
		await fillIndexer(page);
		await continueToFinalStep(page);
		expectNoCalls(vault.traffic);

		await clickButton(page, /edit provider/i);
		await page.getByRole('button', { name: 'Continue' }).click();
		await continueToFinalStep(page);
		await clickButton(page, /edit indexer/i);
		await clickButton(page, /continue without indexer/i);
		await expect(page.getByRole('heading', { name: 'Test the connection' })).toBeFocused();
		expectNoCalls(vault.traffic);

		// Only the explicit test action reaches the network.
		await page.getByTestId('byop-test-connection').click();
		await expect(page.getByTestId('byop-save')).toBeEnabled();
		expect(vault.traffic.testBodies).toHaveLength(1);
		expect(vault.traffic.saveBodies).toHaveLength(0);
		expect(vault.traffic.reauthPosts).toBe(0);
	});

	test('RVS-10 edit actions are refused while a test or save is in flight', async ({
		page
	}) => {
		const vault = await routeVault(page, { holdTests: true, holdSaves: true });
		await fillProvider(page, { connections: 7 });
		await fillIndexer(page);
		await continueToFinalStep(page);

		await page.getByTestId('byop-test-connection').click();
		await vault.firstTestStarted;
		const editProvider = page.getByRole('button', { name: /edit provider/i });
		const editIndexer = page.getByRole('button', { name: /edit indexer/i });
		await expect(editProvider).toBeDisabled();
		await expect(editIndexer).toBeDisabled();
		// Synthetic clicks bypass `disabled`; the handler must refuse too.
		await editProvider.dispatchEvent('click');
		await editIndexer.dispatchEvent('click');
		await expect(page.getByRole('heading', { name: 'Test the connection' })).toBeVisible();
		expect(vault.traffic.testBodies).toHaveLength(1);

		vault.releaseTests();
		await expect(page.getByTestId('byop-save')).toBeEnabled();
		await expect(editProvider).toBeEnabled();

		await page.getByTestId('byop-save').click();
		await vault.firstSaveStarted;
		await expect(editProvider).toBeDisabled();
		await expect(editIndexer).toBeDisabled();
		await editProvider.dispatchEvent('click');
		await editIndexer.dispatchEvent('click');
		await expect(page.getByRole('heading', { name: 'Test the connection' })).toBeVisible();
		expect(vault.traffic.saveBodies).toHaveLength(1);

		vault.releaseSaves();
		await page.waitForURL('/library');
	});

	test('RVS-11 an auth-transition reset removes the summary and edit actions', async ({
		page
	}) => {
		const vault = await routeVault(page);
		await reachTestedFinalStep(page, { withIndexer: true });
		await expectSummary(page, {
			host: HOST,
			port: 563,
			tlsOn: true,
			connections: 7,
			indexerConfigured: true
		});
		await expect(
			page.getByRole('button', { name: /edit provider/i })
		).toBeVisible();

		// A second tab signs out, delivering the auth transition.
		const actor = await page.context().newPage();
		await actor.route('**/v1/vault', (route) =>
			route.fulfill({
				status: 200,
				contentType: 'application/json',
				body: JSON.stringify({})
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

		await expect(
			page.getByText('This setup was reset after an authentication change.')
		).toBeVisible();
		await expect(page.getByText('Step 1 of 3')).toBeVisible();
		await expect(page.getByRole('button', { name: /edit provider/i })).toHaveCount(0);
		await expect(page.getByRole('button', { name: /edit indexer/i })).toHaveCount(0);
		await expect(
			page.getByRole('button', { name: /continue without indexer/i })
		).toHaveCount(0);
		await expect(page.getByLabel('Hostname')).toHaveValue('');
		await expect(page.getByLabel('Password', { exact: true })).toHaveValue('');
		// The reset itself issued no provider test or save for the old draft.
		expect(vault.traffic.testBodies).toHaveLength(1);
		expect(vault.traffic.saveBodies).toHaveLength(0);
	});
});

test.describe('small viewport, color scheme and keyboard', () => {
	for (const colorScheme of ['light', 'dark'] as const) {
		test(`RVS-12 summary and edit work at 320px in ${colorScheme} mode by keyboard`, async ({
			page
		}) => {
			const consoleLines = watchConsole(page);
			await page.setViewportSize({ width: 320, height: 640 });
			await page.emulateMedia({ colorScheme });
			const vault = await routeVault(page);
			await fillProvider(page, { connections: 7 });
			await fillIndexer(page);
			await continueToFinalStep(page);

			await expectSummary(page, {
				host: HOST,
				port: 563,
				tlsOn: true,
				connections: 7,
				indexerConfigured: true
			});
			const overflow = await page.evaluate(() => {
				const doc = document.documentElement;
				return doc.scrollWidth - doc.clientWidth;
			});
			expect(overflow, 'no horizontal overflow at 320px').toBeLessThanOrEqual(0);

			const editIndexer = page.getByRole('button', { name: /edit indexer/i });
			await editIndexer.focus();
			await expect(editIndexer).toBeFocused();
			await page.keyboard.press('Enter');
			await expect(
				page.getByRole('heading', { name: 'Indexer (optional)' })
			).toBeFocused();

			const skip = page.getByRole('button', { name: /continue without indexer/i });
			await skip.focus();
			await expect(skip).toBeFocused();
			await page.keyboard.press('Enter');
			await expect(
				page.getByRole('heading', { name: 'Test the connection' })
			).toBeFocused();
			await expectSummary(page, {
				host: HOST,
				port: 563,
				tlsOn: true,
				connections: 7,
				indexerConfigured: false
			});
			await expectRedacted(page, consoleLines);
			expectNoCalls(vault.traffic);
		});
	}
});
