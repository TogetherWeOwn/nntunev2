import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Test Author-owned acceptance contract. Only the external fetch/credential
 * boundaries are doubled: the real pages, ApiClient, deadlines, response
 * validation, paywall handling and SvelteKit navigation run unchanged.
 *
 * Breaks caught: missing caller-signal forwarding; missing teardown abort or
 * post-await liveness checks (including dependencies that ignore abort); stale
 * auth display/cache writes, paywalls and redirects; bypassed email validity;
 * lost keyboard focus, unassociated errors and button-only busy feedback.
 * Keep corrections with the test author, not in the implementation branch.
 */

type Journey = 'login' | 'signup' | 'magic';
type Stage = 'start' | 'ceremony' | 'finish';
type Outcome = 'success' | 'reject' | 'paywall';
interface BoundaryCall {
	stage: Stage;
	path: string;
	body: Record<string, unknown>;
	hasSignal: boolean;
	aborted: boolean;
}
interface JourneyHarness {
	marker: string;
	pending: () => boolean;
	release: (outcome?: Outcome) => void;
}
declare global {
	interface Window {
		__authJourney: JourneyHarness;
		__authJourneyRecord: (call: BoundaryCall) => Promise<number>;
		__authJourneyAbort: (index: number) => Promise<void>;
		__authJourneyUnhandled: (message: string) => Promise<void>;
	}
}

// Preserve observations even when a broken stale handler reloads the document.
const observed = new WeakMap<Page, { calls: BoundaryCall[]; unhandled: string[] }>();

const EMAIL = 'alice@example.com';
const LOGIN_ERROR = 'Sign-in failed. Try again or use an email link.';
const SIGNUP_ERROR =
	"That didn't work — re-enter the code (and invite code, if you have one) from the email and try again. If it keeps failing, start over to get a fresh code.";
const AUTH_PATHS = {
	login: '/v1/auth/passkey/login',
	signup: '/v1/auth/passkey/register',
	magic: '/v1/auth/magic'
} as const;

/** A held boundary may deliberately ignore an abort, like a late provider. */
async function installHarness(
	page: Page,
	{
		journey,
		hold,
		obeyAbort = false,
		startStatus = 200,
		finishStatus = 200,
		malformedStart = false
	}: {
		journey: Journey;
		hold?: Stage;
		obeyAbort?: boolean;
		startStatus?: number;
		finishStatus?: number;
		malformedStart?: boolean;
	}
): Promise<void> {
	const state = { calls: [] as BoundaryCall[], unhandled: [] as string[] };
	observed.set(page, state);
	await page.exposeFunction('__authJourneyRecord', (call: BoundaryCall) => state.calls.push(call) - 1);
	await page.exposeFunction('__authJourneyAbort', (index: number) => { state.calls[index].aborted = true; });
	await page.exposeFunction('__authJourneyUnhandled', (message: string) => { state.unhandled.push(message); });
	// No request from these mocked journeys may reach a real auth/provider API.
	await page.route('**/v1/**', (route) =>
		route.fulfill({ status: 200, contentType: 'application/json', body: '{}' })
	);
	await page.addInitScript(
		({ journey, hold, obeyAbort, startStatus, finishStatus, malformedStart, paths }) => {
			const nativeFetch = window.fetch.bind(window);
			let release: ((outcome: Outcome) => void) | undefined;
			let held = false;
			window.addEventListener('unhandledrejection', (event) => {
				void window.__authJourneyUnhandled(String(event.reason));
			});
			window.__authJourney = {
				marker: crypto.randomUUID(),
				pending: () => Boolean(release),
				release: (outcome = 'success') => release?.(outcome)
			};
			async function boundary(
				stage: Stage,
				path: string,
				signal: AbortSignal | null | undefined,
				body: Record<string, unknown> = {}
			): Promise<Outcome> {
				const index = await window.__authJourneyRecord({
					stage, path, body,
					hasSignal: signal instanceof AbortSignal,
					aborted: signal?.aborted ?? false
				});
				signal?.addEventListener('abort', () => {
					void window.__authJourneyAbort(index);
				}, { once: true });
				if (stage !== hold || held) return 'success';
				held = true;
				return new Promise<Outcome>((resolve, reject) => {
					const onAbort = () => {
						release = undefined;
						reject(new DOMException('fixture abort', 'AbortError'));
					};
					release = (outcome) => {
						release = undefined;
						signal?.removeEventListener('abort', onAbort);
						if (outcome === 'reject') {
							reject(
								stage === 'ceremony'
									? new DOMException('fixture dismissal', 'NotAllowedError')
									: new TypeError('fixture transport rejection')
							);
						} else resolve(outcome);
					};
					if (obeyAbort) {
						if (signal?.aborted) onAbort();
						else signal?.addEventListener('abort', onAbort, { once: true });
					}
				});
			}
			class FixtureCredential {
				id = 'fixture-credential';
				rawId = new Uint8Array([1]).buffer;
				type = 'public-key';
				authenticatorAttachment = 'platform';
				response = journey === 'signup'
					? {
							clientDataJSON: new Uint8Array([2]).buffer,
							attestationObject: new Uint8Array([3]).buffer
						}
					: {
							clientDataJSON: new Uint8Array([2]).buffer,
							authenticatorData: new Uint8Array([3]).buffer,
							signature: new Uint8Array([4]).buffer,
							userHandle: null
						};
				getClientExtensionResults() {
					return {};
				}
			}
			Object.defineProperty(window, 'PublicKeyCredential', { value: FixtureCredential });
			Object.defineProperty(navigator, 'credentials', {
				value: {
					get: async (options: CredentialRequestOptions) => {
						await boundary('ceremony', 'credentials.get', options?.signal);
						return new FixtureCredential();
					},
					create: async (options: CredentialCreationOptions) => {
						await boundary('ceremony', 'credentials.create', options?.signal);
						return new FixtureCredential();
					}
				}
			});
			window.fetch = async (input, init) => {
				const url = new URL(
					input instanceof Request ? input.url : String(input),
					window.location.href
				);
				const base = paths[journey];
				if (url.pathname !== `${base}/start` && url.pathname !== `${base}/finish`) {
					return nativeFetch(input, init);
				}
				const stage = url.pathname.endsWith('/start') ? 'start' : 'finish';
				const signal = init?.signal ?? (input instanceof Request ? input.signal : undefined);
				const rawBody = init?.body ?? (input instanceof Request ? await input.text() : '{}');
				const outcome = await boundary(stage, url.pathname, signal, JSON.parse(String(rawBody)));
				const status = outcome === 'paywall' ? 429 : stage === 'finish' ? finishStatus : startStatus;
				let body: Record<string, unknown> = { ok: true };
				if (status === 429) body = { code: 'rate_limited' };
				else if (status !== 200) body = { error: 'check your email and try again' };
				else if (stage === 'start' && journey !== 'magic') {
					body = malformedStart
						? { challenge: 17 }
						: journey === 'login'
							? { challenge: 'Y2hhbGxlbmdl', allow_credentials: [], rp_id: 'localhost' }
							: { challenge: 'Y2hhbGxlbmdl', user_id: 'dXNlcg', rp_id: 'localhost' };
				}
				return new Response(JSON.stringify(body), {
					status,
					headers: { 'content-type': 'application/json', ...(status === 429 ? { 'Retry-After': '300' } : {}) }
				});
			};
		},
		{ journey, hold, obeyAbort, startStatus, finishStatus, malformedStart, paths: AUTH_PATHS }
	);
}

function emailField(page: Page, journey: Journey): Locator {
	return journey === 'signup'
		? page.getByTestId('signup-email')
		: page.locator('input[type="email"]');
}
function startButton(page: Page, journey: Journey): Locator {
	return journey === 'signup'
		? page.getByTestId('signup-submit')
		: page.getByRole('button', {
				name: journey === 'magic' ? /email.*link|send.*link/i : /sign in with passkey/i
			});
}
async function calls(page: Page): Promise<BoundaryCall[]> {
	return observed.get(page)!.calls.map((call) => ({ ...call }));
}
async function begin(page: Page, journey: Journey, stage: Stage): Promise<void> {
	await page.goto(journey === 'signup' ? '/signup' : '/login');
	await emailField(page, journey).fill(EMAIL);
	await startButton(page, journey).click();
	if (journey === 'signup' && stage !== 'start') {
		await page.getByLabel('Email code').fill('fixture-code');
		await page.getByRole('button', { name: /finish creating account/i }).click();
	}
	await expect.poll(() => page.evaluate(() => window.__authJourney.pending())).toBe(true);
}

/** SvelteKit intercepts this same-origin anchor; document reloads fail marker. */
async function clientNavigate(page: Page, href: string): Promise<void> {
	const marker = await page.evaluate(() => window.__authJourney.marker);
	await page.evaluate((target) => {
		const link = document.createElement('a');
		link.href = target;
		link.textContent = 'Fixture client navigation';
		link.id = 'auth-journey-navigation';
		document.body.append(link);
	}, href);
	await page.locator('#auth-journey-navigation').click();
	await expect(page).toHaveURL(new RegExp(`${href}$`));
	await expect.poll(() => page.evaluate(() => window.__authJourney.marker)).toBe(marker);
	await page.locator('#auth-journey-navigation').evaluate((link) => link.remove());
}

async function seedNewVisit(page: Page): Promise<void> {
	await page.evaluate(() => {
		sessionStorage.setItem('nntune:signed-in-email', 'new-visit@example.com');
		for (const storage of [sessionStorage, localStorage]) {
			storage.setItem('nntune.library.auth-context.v1', 'new-visit-context');
			const snapshot = JSON.stringify({ savedAt: new Date().toISOString(), items: [{ id: 'new-visit-track', title: 'New visit song' }] });
			storage.setItem('nntune.library.snapshot.v1', snapshot);
			storage.setItem('nntune.library.snapshot.v1.new-visit-context', snapshot);
		}
	});
}
async function storedState(page: Page) {
	return page.evaluate(() =>
		[sessionStorage, localStorage].map((storage) =>
			Object.fromEntries(Object.keys(storage).filter((key) => key.startsWith('nntune')).sort().map((key) => [key, storage.getItem(key)]))
		)
	);
}

/** Drain immediate mock responses, promise continuations and Svelte rendering. */
async function settle(page: Page): Promise<void> {
	try {
		await page.evaluate(async () => {
			await new Promise<void>((resolve) => setTimeout(resolve, 0));
			await new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
		});
	} catch (error) {
		// A stale hard redirect is an assertion failure below, not a setup error.
		if (!String(error).includes('Execution context was destroyed')) throw error;
		await page.waitForLoadState('domcontentloaded');
	}
}

const BOUNDARIES: Array<{ journey: Journey; stage: Stage }> = [
	{ journey: 'login', stage: 'start' },
	{ journey: 'login', stage: 'ceremony' },
	{ journey: 'login', stage: 'finish' },
	{ journey: 'signup', stage: 'start' },
	{ journey: 'signup', stage: 'ceremony' },
	{ journey: 'signup', stage: 'finish' },
	{ journey: 'magic', stage: 'start' }
];

for (const { journey, stage } of BOUNDARIES) {
	for (const outcome of ['success', 'reject', ...(stage !== 'ceremony' ? ['paywall'] : [])] as Outcome[]) {
		test(`${journey} ${stage}: teardown aborts and ignores late ${outcome} after client navigation`, async ({ page }) => {
			const errors: string[] = [];
			page.on('pageerror', (error) => errors.push(error.message));
			await installHarness(page, { journey, hold: stage });
			await begin(page, journey, stage);
			const before = await calls(page);
			expect(before.at(-1)?.aborted).toBe(false);

			await clientNavigate(page, '/pricing');
			const aborted = (await calls(page)).at(-1);
			expect.soft(aborted, 'the pending boundary needs a route-owned, now-aborted signal').toMatchObject({ hasSignal: true, aborted: true });
			// Revisit while the OLD async handler is still pending. An old result
			// must not hijack this fresh instance or overwrite its auth context.
			const destination = journey === 'signup' ? '/signup' : '/login';
			await clientNavigate(page, destination);
			await emailField(page, journey).fill('new-visit@example.com');
			await seedNewVisit(page);
			const storage = await storedState(page);
			await page.evaluate((result) => window.__authJourney.release(result), outcome);
			await settle(page);

			expect.soft((await calls(page)).map(({ stage, path }) => ({ stage, path })), 'stale completion launched another boundary').toEqual(before.map(({ stage, path }) => ({ stage, path })));
			expect.soft(await storedState(page), 'stale completion changed display/library cache state').toEqual(storage);
			await expect.soft(page.getByRole('dialog')).toHaveCount(0);
			await expect.soft(page.getByRole('alert')).toHaveCount(0);
			await expect.soft(emailField(page, journey)).toHaveValue('new-visit@example.com');
			expect.soft(new URL(page.url()).pathname, 'stale completion redirected the new visit').toBe(destination);
			expect.soft(observed.get(page)!.unhandled).toEqual([]);
			expect.soft(errors).toEqual([]);
		});
	}

	test(`${journey} ${stage}: abort-aware pending work rejects safely on teardown`, async ({ page }) => {
		const errors: string[] = [];
		page.on('pageerror', (error) => errors.push(error.message));
		await installHarness(page, { journey, hold: stage, obeyAbort: true });
		await begin(page, journey, stage);
		const before = (await calls(page)).map(({ path }) => path);
		await clientNavigate(page, '/pricing');
		expect.soft((await calls(page)).at(-1)).toMatchObject({ hasSignal: true, aborted: true });
		expect.soft(await page.evaluate(() => window.__authJourney.pending()), 'aborting the route must settle an abort-aware boundary').toBe(false);
		// Also clean up a broken baseline that never supplied/aborted the signal.
		await page.evaluate(() => window.__authJourney.release('reject'));
		await settle(page);
		expect((await calls(page)).map(({ path }) => path)).toEqual(before);
		expect(new URL(page.url()).pathname).toBe('/pricing');
		await expect(page.getByRole('dialog')).toHaveCount(0);
		expect(observed.get(page)!.unhandled).toEqual([]);
		expect(errors).toEqual([]);
	});
}

for (const email of ['', 'not-an-email', 'alice@']) {
	test(`login: keyboard passkey activation rejects ${JSON.stringify(email)} and focuses email validity`, async ({ page }) => {
		await installHarness(page, { journey: 'login', hold: 'start' });
		await page.goto('/login');
		await emailField(page, 'login').fill(email);
		await startButton(page, 'login').focus();
		await page.keyboard.press('Enter');
		await settle(page);
		expect.soft(await calls(page), 'invalid email must not start network or credentials').toEqual([]);
		await expect(emailField(page, 'login')).toBeFocused();
		expect(await emailField(page, 'login').evaluate((input) => (input as HTMLInputElement).validity.valid)).toBe(false);
		await expect(page.getByText(LOGIN_ERROR)).toHaveCount(0);
	});
}

async function expectDescription(field: Locator, text: RegExp | string): Promise<void> {
	const description = await field.evaluate((element) => {
		const ids = element.getAttribute('aria-describedby')?.split(/\s+/).filter(Boolean) ?? [];
		return ids.map((id) => document.getElementById(id))
			.filter((node) => node && node.getClientRects().length > 0)
			.map((node) => node?.textContent ?? '').join(' ');
	});
	const assertion = expect(description, 'aria-describedby must resolve to relevant visible guidance/error text');
	if (typeof text === 'string') assertion.toContain(text);
	else assertion.toMatch(text);
}

async function codeStep(page: Page): Promise<void> {
	await page.goto('/signup');
	await emailField(page, 'signup').fill(EMAIL);
	await startButton(page, 'signup').focus();
	await page.keyboard.press('Enter');
	await expect(page.getByLabel('Email code')).toBeVisible();
}

test('signup: keyboard start focuses the code and associates the emailed-code guidance', async ({ page }) => {
	await installHarness(page, { journey: 'signup' });
	await codeStep(page);
	const code = page.getByLabel('Email code');
	await expect.soft(code).toBeFocused();
	await expectDescription(code, /email|code/i);
	expect((await calls(page)).map(({ stage }) => stage)).toEqual(['start']);
});

test('signup: keyboard Start over restores email focus and clears codes without a request', async ({ page }) => {
	await installHarness(page, { journey: 'signup' });
	await codeStep(page);
	await page.getByLabel('Email code').fill('discarded-code');
	await page.getByLabel(/invite code/i).fill('discarded-invite');
	await page.getByRole('button', { name: /start over/i }).focus();
	await page.keyboard.press('Enter');
	await expect(emailField(page, 'signup')).toBeVisible();
	await expect.soft(emailField(page, 'signup')).toBeFocused();
	expect((await calls(page)).map(({ stage }) => stage)).toEqual(['start']);
	await startButton(page, 'signup').click();
	await expect(page.getByLabel('Email code')).toHaveValue('');
	await expect(page.getByLabel(/invite code/i)).toHaveValue('');
});

test('signup: a start error is associated with the email field without enumerating accounts', async ({ page }) => {
	await installHarness(page, { journey: 'signup', startStatus: 401 });
	await page.goto('/signup');
	const email = emailField(page, 'signup');
	await email.fill(EMAIL);
	await startButton(page, 'signup').click();
	const alert = page.getByRole('alert');
	await expect(alert).toBeVisible();
	await expect(alert).not.toContainText(/account exists|unknown account|registered account/i);
	await expect.soft(email).toHaveAttribute('aria-invalid', 'true');
	await expectDescription(email, (await alert.innerText()).trim());
	expect((await calls(page)).map(({ stage }) => stage)).toEqual(['start']);
});

test('signup: a non-enumerating code error is associated with the code field', async ({ page }) => {
	await installHarness(page, { journey: 'signup', finishStatus: 401 });
	await codeStep(page);
	const code = page.getByLabel('Email code');
	await code.fill('wrong-code');
	await page.getByRole('button', { name: /finish creating account/i }).click();
	await expect(page.getByRole('alert')).toHaveText(SIGNUP_ERROR);
	await expect.soft(code).toHaveAttribute('aria-invalid', 'true');
	await expectDescription(code, /re-enter the code|didn't work/i);
	expect((await calls(page)).filter(({ stage }) => stage === 'finish')).toHaveLength(1);
});

for (const stage of ['start', 'ceremony', 'finish'] as const) {
	test(`signup: ${stage} announces working state outside button text`, async ({ page }) => {
		await installHarness(page, { journey: 'signup', hold: stage });
		await begin(page, 'signup', stage);
		const statuses = await page.locator('[role="status"], [role="alert"], [aria-live="polite"], [aria-live="assertive"]').evaluateAll((elements) =>
			elements.filter((element) => !element.closest('button') && element.getClientRects().length > 0).map((element) => element.textContent ?? '').join(' ')
		);
		expect(statuses, 'busy work must be announced, not just rename the disabled button').toMatch(/sending|working|creating|finishing|wait|passkey/i);
	});
}

for (const journey of ['login', 'signup'] as const) {
	test(`${journey}: live attempt still succeeds once and clears the previous auth snapshot`, async ({ page }) => {
		await installHarness(page, { journey, hold: 'finish' });
		await begin(page, journey, 'finish');
		await seedNewVisit(page);
		expect((await calls(page)).map(({ stage }) => stage)).toEqual(['start', 'ceremony', 'finish']);
		await page.evaluate(() => window.__authJourney.release());
		await expect(page).toHaveURL(/\/onboarding\/byop$/);
		expect(await page.evaluate(() => sessionStorage.getItem('nntune:signed-in-email'))).toBe(EMAIL);
		for (const storage of await storedState(page)) {
			expect(storage['nntune.library.snapshot.v1']).toBeUndefined();
			expect(storage['nntune.library.snapshot.v1.new-visit-context']).toBeUndefined();
		}
	});

	test(`${journey}: malformed start stops before credentials and keeps generic failure classification`, async ({ page }) => {
		await installHarness(page, { journey, malformedStart: true });
		await page.goto(journey === 'signup' ? '/signup' : '/login');
		await emailField(page, journey).fill(EMAIL);
		await startButton(page, journey).click();
		await expect(page.getByRole('alert')).toBeVisible();
		expect((await calls(page)).map(({ stage }) => stage)).toEqual(['start']);
		await expect(page.getByRole('alert')).not.toContainText(/challenge|undefined|TypeError|fixture/i);
		await expect(page.getByRole('dialog')).toHaveCount(0);
	});
}
