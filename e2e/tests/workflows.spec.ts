import { test, expect } from '@playwright/test';
import { readFileSync, readdirSync, mkdtempSync, rmSync, writeFileSync, mkdirSync, existsSync, symlinkSync, chmodSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import config from '../playwright.config.js';

const workflowDir = new URL('../../.github/workflows/', import.meta.url);
const stagingWorkflow = readFileSync(new URL('e2e-staging.yml', workflowDir), 'utf8');

// Execute the exact checked-in shell blocks, not a copy of their logic. Only
// dummy inputs enter the child; no ambient credentials or network requests.
function runShell(script: string, env: Record<string, string> = {}, cwd?: string) {
  const scratch = mkdtempSync(join(process.env.PAPERCLIP_RUN_SCRATCH_DIR || tmpdir(), 'workflow-test-'));
  const outputFile = join(scratch, 'github-output');
  try {
    const result = spawnSync('/bin/bash', ['--noprofile', '--norc', '-e', '-o', 'pipefail', '-c', script], {
      encoding: 'utf8',
      timeout: 5_000,
      ...(cwd ? { cwd } : {}),
      env: { PATH: process.env.PATH, GITHUB_OUTPUT: outputFile, ...env },
    });
    expect(result.error).toBeUndefined();
    expect(result.signal).toBeNull();
    let output = result.stdout + result.stderr;
    // Failing preflights intentionally never create a configured output.
    try { output += readFileSync(outputFile, 'utf8'); } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
    }
    return { status: result.status, output };
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
}

const preflights = [...stagingWorkflow.matchAll(
  /      - name: Check staging configuration\n([\s\S]*?)        run: \|\n((?:          [^\n]*\n)+)/g,
)];

const dummyPair = {
  CF_ACCESS_CLIENT_ID: 'dummy-client-id',
  CF_ACCESS_CLIENT_SECRET: 'dummy-client-secret',
};

test('both staging jobs have tested preflights', () => {
  expect(preflights).toHaveLength(2);
});

for (const [index, preflight] of preflights.entries()) {
  const script = preflight[2]!.replace(/^ {10}/gm, '');
  test.describe(index === 0 ? 'synthetic preflight' : 'journey preflight', () => {
    test('binds both Access secrets into the preflight environment', () => {
      expect(preflight[1]).toContain('CF_ACCESS_CLIENT_ID: ${{ secrets.CF_ACCESS_CLIENT_ID }}');
      expect(preflight[1]).toContain('CF_ACCESS_CLIENT_SECRET: ${{ secrets.CF_ACCESS_CLIENT_SECRET }}');
    });

    test('unconfigured staging skips offline', () => {
      const result = runShell(script);
      expect(result.status).toBe(0);
      expect(result.output).toContain('configured=false');
      expect(result.output).not.toContain('configured=true');
    });

    for (const [name, pair] of [
      ['both missing', {}],
      ['ID only', { CF_ACCESS_CLIENT_ID: dummyPair.CF_ACCESS_CLIENT_ID }],
      ['secret only', { CF_ACCESS_CLIENT_SECRET: dummyPair.CF_ACCESS_CLIENT_SECRET }],
    ] as const) {
      test(`configured staging rejects ${name} with names-only output`, () => {
        const result = runShell(script, { STAGING_BASE_URL: 'https://staging.example', ...pair });
        expect(result.status).not.toBe(0);
        expect(result.output).toContain('CF_ACCESS_CLIENT_ID');
        expect(result.output).toContain('CF_ACCESS_CLIENT_SECRET');
        expect(result.output).not.toContain('configured=true');
        expect(result.output).not.toContain(dummyPair.CF_ACCESS_CLIENT_ID);
        expect(result.output).not.toContain(dummyPair.CF_ACCESS_CLIENT_SECRET);
      });
    }

    test('configured staging with complete dummy pair proceeds', () => {
      const result = runShell(script, { STAGING_BASE_URL: 'https://staging.example', ...dummyPair });
      expect(result.status).toBe(0);
      expect(result.output).toContain('configured=true');
      expect(result.output).not.toContain(dummyPair.CF_ACCESS_CLIENT_ID);
      expect(result.output).not.toContain(dummyPair.CF_ACCESS_CLIENT_SECRET);
    });
  });
}

const ciWorkflow = readFileSync(new URL('ci.yml', workflowDir), 'utf8');
const aggregate = ciWorkflow.split('  ci-ok:\n')[1]?.match(/      - run: \|\n((?:          [^\n]*\n)+)/)?.[1];

test.describe('ci-ok aggregate', () => {
  for (const result of ['success', 'failure', 'cancelled', 'skipped']) {
    test(`e2e-lint ${result} ${result === 'success' ? 'passes' : 'fails'} the aggregate`, () => {
      expect(aggregate).toBeDefined();
      const needs = JSON.stringify({ placeholder: { result: 'success' }, 'e2e-lint': { result } }, null, 2);
      const script = aggregate!.replace(/^ {10}/gm, '');
      const actual = runShell(script, { NEEDS_JSON: needs });
      expect(actual.status).toBe(result === 'success' ? 0 : 1);
    });
  }
});

test('credential-bearing contexts do not record raw traces or screenshots', () => {
  expect(config.use?.trace).toBe('off');
  expect(config.use?.screenshot).toBe('off');
});

test('every workflow Action ref is a full commit SHA', () => {
  for (const name of readdirSync(fileURLToPath(workflowDir)).filter(name => name.endsWith('.yml'))) {
    const workflow = readFileSync(new URL(name, workflowDir), 'utf8');
    for (const match of workflow.matchAll(/uses:\s+[^\s@]+@([^\s#]+)/g)) {
      expect(match[1], `${name}: ${match[0]}`).toMatch(/^[a-f0-9]{40}$/);
    }
  }
});

test.describe('credential scope (static pins)', () => {
  test('no context-level extraHTTPHeaders in the Playwright config', () => {
    // Usage patterns only (colons/parens): explanatory comments may name the
    // APIs to document why they are banned.
    const configSource = readFileSync(new URL('../playwright.config.ts', import.meta.url), 'utf8');
    expect(configSource).not.toContain('extraHTTPHeaders:');
    expect(configSource).not.toContain('setExtraHTTPHeaders(');
  });

  test('browser auth is origin-scoped through a context route', () => {
    const accounts = readFileSync(new URL('../helpers/accounts.ts', import.meta.url), 'utf8');
    expect(accounts).toContain('isStagingOrigin');
    expect(accounts).toContain('context.route');
    expect(accounts).not.toContain('extraHTTPHeaders:');
    expect(accounts).not.toContain('setExtraHTTPHeaders(');
  });

  test('credentialed API calls never follow redirects (maxRedirects: 0)', () => {
    const accounts = readFileSync(new URL('../helpers/accounts.ts', import.meta.url), 'utf8');
    expect(accounts).toContain('maxRedirects: 0');
    expect(accounts).toContain('export async function apiGet');
    expect(accounts).toContain('export async function apiPost');
    expect(accounts).toContain('export async function apiOptions');
    expect(accounts).toContain('unexpected redirect');
  });

  // Raw API-context calls follow a staging 302 to another origin WITH the
  // Access pair + bearer (Playwright follows up to 20 redirects by default).
  // Live specs must go through apiGet/apiPost; helpers.spec.ts may use the
  // raw fixture inside its own no-follow proof (it asserts the wrapper, not
  // staging). The pin bans every method (get/post/put/patch/delete/head/fetch
  // on request/page.request/context.request) without a headers condition: the
  // old /request\.(get|post)\([^)]*headers/ stopped at the first ')' so a URL
  // containing a nested call (every API.stream() stream URL) never matched,
  // and request.fetch bypassed it entirely.
  const RAW_API_CALL = /\b(?:request|page\.request|context\.request)\.(?:get|post|put|patch|delete|head|fetch)\s*\(/;
  test('live specs use the no-follow API wrapper, not raw credentialed calls', () => {
    // Raw request.get/post with headers would follow a staging 302 to another
    // origin WITH the Access pair + bearer. Live specs must go through
    // apiGet/apiPost; helpers.spec.ts may use the raw fixture inside its own
    // no-follow proof (it asserts the wrapper, not staging).
    for (const name of [
      'health.spec.ts',
      'ingest-play-meter.spec.ts',
      'signup-byop.spec.ts',
      'signup-wizard-arrival.spec.ts',
      'login-magic-link.spec.ts',
      'player-queue-order.spec.ts',
      'demo-audio-contract.spec.ts',
      'demo-media-sessionless.spec.ts',
      'served-routes.spec.ts',
      'build-identity.spec.ts',
      'zz-build-identity-final.spec.ts',
      'api-worker-anonymous.spec.ts',
    ]) {
      const source = readFileSync(new URL(`../tests/${name}`, import.meta.url), 'utf8');
      expect(source, `${name} must not carry credentials on a raw request call`).not.toMatch(
        RAW_API_CALL,
      );
    }
    for (const name of [
      'health.spec.ts',
      'ingest-play-meter.spec.ts',
      'player-queue-order.spec.ts',
      'demo-audio-contract.spec.ts',
      'demo-media-sessionless.spec.ts',
      'api-worker-anonymous.spec.ts',
      'served-routes.spec.ts',
    ]) {
      const source = readFileSync(new URL(`../tests/${name}`, import.meta.url), 'utf8');
      expect(source, `${name} must use the no-follow wrapper`).toContain('apiGet(');
    }
    // The build-identity opening + closing legs reach the no-follow wrapper
    // through the shared live helper (pinned in the deployed-build identity
    // section below), never raw: the ban above still applies to both files.
    for (const name of ['build-identity.spec.ts', 'zz-build-identity-final.spec.ts']) {
      const source = readFileSync(new URL(`../tests/${name}`, import.meta.url), 'utf8');
      expect(source, `${name} must route through the shared live helper`).toContain(
        'assertServingBuildMatchesPromotion(',
      );
    }
  });

  test('anonymous denial probe stays credential-free and no-follow', () => {
    // The anonymous app-shell check must go through anonymousGet (no headers
    // parameter, maxRedirects: 0) and the inverse wall assertion. Pin both
    // so it cannot silently regress to a credentialed apiGet (which would
    // prove the with-token side again) or to a raw call.
    const accounts = readFileSync(new URL('../helpers/accounts.ts', import.meta.url), 'utf8');
    expect(accounts).toContain('export async function anonymousGet');
    const health = readFileSync(new URL(`../tests/health.spec.ts`, import.meta.url), 'utf8');
    expect(health, 'health.spec.ts must probe anonymously via anonymousGet').toContain(
      'anonymousGet(',
    );
    expect(health, 'health.spec.ts must assert the Access wall for the anonymous probe').toContain(
      'assertAccessWall(',
    );
    expect(health, 'anonymous probe must use an isolated request context').toContain(
      'playwrightRequest.newContext(',
    );
  });

  test('no-follow pin catches nested-paren and fetch mutants', () => {
    // Regression proof for the pin above: the reviewer's bypass mutants must
    // match (fail the pin), while the approved wrapper call must not.
    const nestedStreamCall =
      "await request.get(`${baseURL}${API.stream(trackId as string)}`, " +
      "{ headers: { ...headers, Range: 'bytes=0-8' }, timeout: 30_000 })";
    const fetchCall = 'await request.fetch(libraryUrl, { headers })';
    const pageScoped = 'await page.request.get(`${baseURL}/api/usage`, { headers })';
    expect(nestedStreamCall).toMatch(RAW_API_CALL);
    expect(fetchCall).toMatch(RAW_API_CALL);
    expect(pageScoped).toMatch(RAW_API_CALL);
    expect("await apiGet(request, url, headers, 'range')").not.toMatch(RAW_API_CALL);
  });
});

test.describe('public UI signup coverage (static pin)', () => {
  // Isolate the dedicated case and the shared form helper by slicing from
  // their markers to the next top-level test/export (or EOF), so a mint call
  // added to either segment (the bypass this coverage exists to prevent)
  // fails here even though other tests in the file legitimately mint.
  test('dedicated UI-signup case submits the form with no account-seam mint', () => {
    const spec = readFileSync(new URL('../tests/signup-byop.spec.ts', import.meta.url), 'utf8');
    const marker = "test('public UI signup succeeds even when the test-account seam is configured'";
    const start = spec.indexOf(marker);
    expect(start, 'dedicated public UI signup case is missing').toBeGreaterThan(-1);
    const rest = spec.slice(start + marker.length);
    const nextTest = rest.search(/\ntest(?:\(|\.describe)/);
    const dedicated = nextTest === -1 ? spec.slice(start) : spec.slice(start, start + marker.length + nextTest);
    // Null identity = Access pair only: the page starts unauthenticated, so
    // the form is the only account-creation path even with QA_AUTH_TOKEN set.
    expect(dedicated).toContain('applyAccountToPage(page, null, baseURL)');
    expect(dedicated).toContain('submitPublicUiSignup(page, baseURL)');
    expect(dedicated).not.toContain('tryMintTestAccount');

    const accounts = readFileSync(new URL('../helpers/accounts.ts', import.meta.url), 'utf8');
    const helperStart = accounts.indexOf('export async function submitPublicUiSignup');
    expect(helperStart, 'shared public UI signup helper is missing').toBeGreaterThan(-1);
    const helperRest = accounts.slice(helperStart + 'export async function submitPublicUiSignup'.length);
    const nextExport = helperRest.search(/\nexport /);
    const helper = nextExport === -1 ? accounts.slice(helperStart) : accounts.slice(helperStart, helperStart + 'export async function submitPublicUiSignup'.length + nextExport);
    expect(helper).toContain('uniqueTestEmail(');
    expect(helper).toContain('`${baseURL}/signup`');
    expect(helper).toContain('TESTIDS.signupEmail');
    expect(helper).toContain('TESTIDS.signupSubmit');
    expect(helper).toContain('TESTIDS.signupError');
    // The real page is two-step: the helper asserts the emailed-code step
    // (magic_code form), never wizard arrival — asserting the wizard without
    // the code-retrieval seam reds every configured run.
    expect(helper).toContain('magic_code');
    expect(helper).not.toContain('TESTIDS.byopHost');
    expect(helper).not.toContain("'/onboarding/byop'");
    expect(helper).not.toContain('tryMintTestAccount');
    expect(helper).not.toContain('API.testAccounts');
    expect(helper).not.toContain('qaAuthHeader');
  });

  // Wizard-arrival leg: the new spec proves the real form reaches
  // `/onboarding/byop` via the virtual authenticator + the staging-only code
  // seam. These pins keep it honest offline: still no mint, still skip-green
  // pre-deploy, still names-only diagnostics — while the wizard assertion
  // lives in the NEW helper (the shared helper above must keep asserting the
  // `magic_code` step only, never the wizard).
  test('wizard-arrival case reaches the wizard with no account-seam mint', () => {
    const spec = readFileSync(new URL('../tests/signup-wizard-arrival.spec.ts', import.meta.url), 'utf8');
    const marker = "test('public UI signup with virtual authenticator reaches the BYOP wizard'";
    const start = spec.indexOf(marker);
    expect(start, 'wizard-arrival case is missing').toBeGreaterThan(-1);
    const rest = spec.slice(start + marker.length);
    const nextTest = rest.search(/\ntest(?:\(|\.describe)/);
    const dedicated = nextTest === -1 ? spec.slice(start) : spec.slice(start, start + marker.length + nextTest);
    // Null identity = Access pair only: the page starts unauthenticated, so
    // the form is the only account-creation path even with QA_AUTH_TOKEN set.
    expect(dedicated).toContain('applyAccountToPage(page, null, baseURL)');
    expect(dedicated).toContain('submitPublicUiSignupToByopWizard(page, request, baseURL)');
    // Arrival asserted at the spec level too (landed URL + wizard field).
    expect(dedicated).toContain("'/onboarding/byop'");
    expect(dedicated).toContain('page.url()');
    expect(dedicated).toContain('TESTIDS.byopHost');
    // Never the mint seam: arrival must come from the public form.
    expect(dedicated).not.toContain('tryMintTestAccount');
    expect(dedicated).not.toContain('API.testAccounts');
    // Skip-green pre-deploy (file-level + in-test guards); QA_AUTH_TOKEN
    // guard skips locally and fails closed on CI.
    expect(spec).toContain('STAGING_BASE_URL');
    expect(dedicated).toContain('QA_AUTH_TOKEN');
    // Provenance label without account data.
    expect(dedicated).toContain('annotateSignupPath');
  });

  test('wizard-arrival helper completes the form via the code seam and virtual authenticator', () => {
    const accounts = readFileSync(new URL('../helpers/accounts.ts', import.meta.url), 'utf8');
    const helperStart = accounts.indexOf('export async function submitPublicUiSignupToByopWizard');
    expect(helperStart, 'wizard-arrival helper is missing').toBeGreaterThan(-1);
    const helperRest = accounts.slice(helperStart + 'export async function submitPublicUiSignupToByopWizard'.length);
    const nextExport = helperRest.search(/\nexport /);
    const helper = nextExport === -1 ? accounts.slice(helperStart) : accounts.slice(helperStart, helperStart + 'export async function submitPublicUiSignupToByopWizard'.length + nextExport);
    // Real form, both steps: email submit then the emailed-code field.
    expect(helper).toContain('`${baseURL}/signup`');
    expect(helper).toContain('TESTIDS.signupEmail');
    expect(helper).toContain('TESTIDS.signupSubmit');
    expect(helper).toContain('magic_code');
    // Code comes from the staging-only seam (never minted, never fabricated).
    // The seam path itself (`API.testCodes`) is pinned on the seam reader
    // below plus the contract — this helper reaches it via the reader.
    expect(helper).toContain('retrieveStagedSignupCode');
    // Passkey finish runs against a virtual authenticator, not hardware
    // (the CDP call lives in `enableVirtualAuthenticator`, pinned below).
    expect(helper).toContain('enableVirtualAuthenticator');
    // Wizard arrival asserted here (unlike the shared helper above).
    expect(helper).toContain("'/onboarding/byop'");
    expect(helper).toContain('page.url()');
    expect(helper).toContain('TESTIDS.byopHost');
    // Never the mint seam in the arrival path.
    expect(helper).not.toContain('tryMintTestAccount');
    expect(helper).not.toContain('API.testAccounts');
    // Names-only diagnostics: neither the mailbox nor the code value may be
    // interpolated into an error.
    expect(helper).toContain('no values logged');
    expect(helper).not.toContain('${email}');
    expect(helper).not.toContain('${code}');

    // The seam reader itself: token-gated, fail-closed, names-only.
    const seamStart = accounts.indexOf('export async function retrieveStagedSignupCode');
    expect(seamStart, 'staged code-seam reader is missing').toBeGreaterThan(-1);
    const seamRest = accounts.slice(seamStart + 'export async function retrieveStagedSignupCode'.length);
    const seamNext = seamRest.search(/\nexport /);
    const seam = seamNext === -1 ? accounts.slice(seamStart) : accounts.slice(seamStart, seamStart + 'export async function retrieveStagedSignupCode'.length + seamNext);
    expect(seam).toContain('QA_AUTH_TOKEN');
    expect(seam).toContain('API.testCodes');
    expect(seam).toContain('qaAuthHeader');
    expect(seam).toContain('magic_token');
    expect(seam).toContain('HTTP ${status}');
    expect(seam).toContain('no values logged');
    expect(seam).not.toContain('${email}');
    expect(seam).not.toContain('tryMintTestAccount');
    expect(seam).not.toContain('API.testAccounts');

    // The virtual authenticator: CDP WebAuthn domain, resident-key plus
    // user-verification capable and pre-verified, matching the signup page's
    // `required` ceremony. Failures stay names-only.
    const vaStart = accounts.indexOf('export async function enableVirtualAuthenticator');
    expect(vaStart, 'virtual-authenticator helper is missing').toBeGreaterThan(-1);
    const vaRest = accounts.slice(vaStart + 'export async function enableVirtualAuthenticator'.length);
    const vaNext = vaRest.search(/\nexport /);
    const va = vaNext === -1 ? accounts.slice(vaStart) : accounts.slice(vaStart, vaStart + 'export async function enableVirtualAuthenticator'.length + vaNext);
    expect(va).toContain('WebAuthn.enable');
    expect(va).toContain('WebAuthn.addVirtualAuthenticator');
    expect(va).toContain('hasResidentKey');
    expect(va).toContain('hasUserVerification');
    expect(va).toContain('isUserVerified');
    expect(va).toContain('newCDPSession');
    expect(va).toContain('no values logged');

    // The contract carries the seam path so a rename fails by name.
    const contract = readFileSync(new URL('../helpers/contract.ts', import.meta.url), 'utf8');
    expect(contract).toContain("testCodes: '/api/test-codes'");
  });
});

test.describe('returning-user login coverage (static pin)', () => {
  // The login spec proves a minted (existing) account signs in through the
  // real `/login` form, redeeming the staged code on the `/auth/magic`
  // confirm page. These pins keep it honest offline: still seam-backed
  // where a seam is needed, still skip-green pre-deploy, still names-only
  // diagnostics — while the generic sent/error states stay asserted at
  // runtime in the spec, not as source text here.
  test('login live case mints the returning account and redeems via the code seam', () => {
    const spec = readFileSync(new URL('../tests/login-magic-link.spec.ts', import.meta.url), 'utf8');
    const marker = "test('returning user signs in with an emailed magic link and lands on provider setup'";
    const start = spec.indexOf(marker);
    expect(start, 'returning-user login case is missing').toBeGreaterThan(-1);
    const rest = spec.slice(start + marker.length);
    const nextTest = rest.search(/\ntest(?:\(|\.describe)/);
    const live = nextTest === -1 ? spec.slice(start) : spec.slice(start, start + marker.length + nextTest);
    // The returning account comes from the mint seam; the code from the
    // retrieval seam through the shared reader (never fabricated, never a
    // raw endpoint call).
    expect(live).toContain('tryMintTestAccount(request, baseURL)');
    expect(live).toContain('retrieveStagedSignupCode(request, baseURL');
    // Null identity = Access pair only: the page starts unauthenticated, so
    // the form is the only sign-in path even with QA_AUTH_TOKEN set.
    expect(live).toContain('applyAccountToPage(page, null, baseURL)');
    // Real form plus the explicit-confirm landing: email submit, generic
    // sent state, confirm redeem, provider-setup arrival as the session
    // proof (a failed finish stays on `/auth/magic`).
    expect(live).toContain('`${baseURL}/login`');
    expect(live).toContain('input[name="email"]');
    expect(live).toContain('check your inbox');
    expect(live).toContain('`${baseURL}/auth/magic?token=');
    expect(live).toContain('sign in with this link');
    expect(live).toContain("'/onboarding/byop'");
    // Skip-green pre-deploy (file-level + in-test guards); the seam-token
    // guard skips locally and fails closed on CI.
    expect(spec).toContain('STAGING_BASE_URL');
    expect(live).toContain('requireSeamToken(');
    expect(spec).toContain('QA_AUTH_TOKEN');
    // Never raw endpoint or header access in the spec: both seams stay
    // behind their helpers (which carry the fail-closed + names-only
    // contract pinned above).
    expect(live).not.toContain('API.testAccounts');
    expect(live).not.toContain('API.testCodes');
    expect(live).not.toContain('qaAuthHeader');
    // Names-only diagnostics: neither the mailbox nor the code value may be
    // interpolated into an error or assertion message.
    expect(live).not.toContain('${email}');
    expect(live).not.toContain('${code}');
    expect(live).not.toContain('${minted');
  });

  test('login generic states are asserted at runtime with no seam mint', () => {
    const spec = readFileSync(new URL('../tests/login-magic-link.spec.ts', import.meta.url), 'utf8');
    const sentMarker = "test('login shows the same generic sent state for an unknown address'";
    const sentStart = spec.indexOf(sentMarker);
    expect(sentStart, 'login unknown-address case is missing').toBeGreaterThan(-1);
    const sentRest = spec.slice(sentStart + sentMarker.length);
    const sentNext = sentRest.search(/\ntest(?:\(|\.describe)/);
    const sent = sentNext === -1 ? spec.slice(sentStart) : spec.slice(sentStart, sentStart + sentMarker.length + sentNext);
    // Never-registered mailbox, same sent copy: a distinct answer here
    // would be an enumeration oracle. No seam is needed — the API answers
    // the same shape for unknown addresses.
    expect(sent).toContain("uniqueTestEmail('e2e-unknown')");
    expect(sent).toContain('check your inbox');
    expect(sent).toContain('applyAccountToPage(page, null, baseURL)');
    expect(sent).not.toContain('tryMintTestAccount');
    expect(sent).not.toContain('retrieveStagedSignupCode');
    expect(sent).not.toContain('QA_AUTH_TOKEN');

    const errMarker = "test('login surfaces a generic error when the magic-link request fails'";
    const errStart = spec.indexOf(errMarker);
    expect(errStart, 'login error-state case is missing').toBeGreaterThan(-1);
    const errRest = spec.slice(errStart + errMarker.length);
    const errNext = errRest.search(/\ntest(?:\(|\.describe)/);
    const err = errNext === -1 ? spec.slice(errStart) : spec.slice(errStart, errStart + errMarker.length + errNext);
    // Forced backend failure is a local route stub, never a staging outage
    // and never a seam call — the error state renders with no token.
    expect(err).toContain("page.route('**/v1/auth/magic/start'");
    expect(err).toContain('something went wrong');
    expect(err).not.toContain('tryMintTestAccount');
    expect(err).not.toContain('retrieveStagedSignupCode');
    expect(err).not.toContain('QA_AUTH_TOKEN');
  });
});

// Execute the exact checked-in sanitize blocks: Playwright 1.55 writes
// test-results/**/error-context.md (ARIA snapshots rendering filled input
// values, including the BYOP password) and the junit reporter embeds failure
// messages (e.g. toHaveValue Received strings) carrying the same filled
// secrets. CI uses the junit reporter only and uploads test-results/ alone;
// the sanitize step deletes error-context.md plus the whole
// playwright-report/ dir, then scrubs secret values from test-results/.
const sanitizeSections = [...stagingWorkflow.matchAll(
  /      - name: Sanitize failure artifacts([\s\S]*?)run: \|\n((?:          [^\n]*\n)+)/g,
)];
const sanitizeBlocks = sanitizeSections.map((m) => m[2]!);

function listFilesRecursive(dir: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) out.push(...listFilesRecursive(full));
    else out.push(full);
  }
  return out;
}

// Scan a file for a marker in plain text AND inside base64 blobs (the html
// report embeds its payload as a base64 zip): decode every long base64 run
// and check the decoded bytes too.
function fileContainsMarker(file: string, marker: string): boolean {
  const raw = readFileSync(file, 'utf8');
  if (raw.includes(marker)) return true;
  for (const match of raw.matchAll(/[A-Za-z0-9+/]{200,}={0,2}/g)) {
    try {
      const decoded = Buffer.from(match[0], 'base64').toString('utf8');
      if (decoded.includes(marker)) return true;
    } catch {
      // Not decodable — ignore this run.
    }
  }
  return false;
}

test.describe('failure-artifact sanitization', () => {
  test('both jobs sanitize before uploading', () => {
    expect(sanitizeSections).toHaveLength(2);
    expect(sanitizeBlocks).toHaveLength(2);
    for (const section of sanitizeSections) {
      // The scrub needs the secret VALUES, so each sanitize step must bind
      // them (names only in source; values come from GitHub secrets).
      for (const name of [
        'CF_ACCESS_CLIENT_ID',
        'CF_ACCESS_CLIENT_SECRET',
        'QA_AUTH_TOKEN',
        'SYNTHETIC_NNTP_HOST',
        'SYNTHETIC_NNTP_USER',
        'SYNTHETIC_NNTP_PASS',
      ]) {
        expect(section[1], `sanitize step env binds ${name}`).toContain(name);
      }
    }
    for (const block of sanitizeBlocks) {
      const script = block.replace(/^ {10}/gm, '');
      expect(script).toContain('error-context.md');
      // Whole-dir removal: the old `playwright-report/data`-only strip left
      // index.html (with its embedded base64 zip) behind.
      expect(script).toContain('rm -rf playwright-report');
      expect(script).not.toContain('playwright-report/data');
      // junit.xml embeds failure messages (toHaveValue Received strings) that
      // can carry filled secrets: the block must scrub test-results/.
      expect(script).toContain('***REDACTED***');
      expect(script).toContain('SYNTHETIC_NNTP_PASS');
    }
    // Each sanitize precedes its job's upload: nothing uploads raw first.
    for (const artifact of ['e2e-synthetic-report', 'e2e-journey-report']) {
      const uploadAt = stagingWorkflow.indexOf(artifact);
      const sanitizeAt = stagingWorkflow.lastIndexOf('Sanitize failure artifacts', uploadAt);
      expect(sanitizeAt, `${artifact} uploads before any sanitize`).toBeGreaterThan(-1);
    }
  });

  test('uploads are junit-only (no playwright-report)', () => {
    // Both jobs upload test-results/ only. The html report (index.html with
    // its embedded base64 zip of step fill values) must never be an upload
    // path — the sanitize step may still NAME playwright-report/ to delete it.
    const uploadPaths = [...stagingWorkflow.matchAll(/path: \|\n((?:            [^\n]*\n)+)/g)];
    expect(uploadPaths).toHaveLength(2);
    for (const upload of uploadPaths) {
      expect(upload[1]).toContain('e2e/test-results/');
      expect(upload[1]).not.toContain('playwright-report');
    }
  });

  test('CI reporter is junit-only (html embeds fill values)', () => {
    const configSource = readFileSync(new URL('../playwright.config.ts', import.meta.url), 'utf8');
    expect(configSource).toContain('junit');
    expect(configSource).toContain('test-results/junit.xml');
    expect(configSource).not.toContain("'html'");
    expect(configSource).not.toContain('"html"');
  });

  // Real-browser proof: run a genuinely failing Playwright test that fills a
  // marker password under the CI reporter config (junit, no html,
  // trace/screenshot off). The junit reporter embeds the failure message
  // (toHaveValue Received string), so junit.xml DOES carry the marker before
  // sanitize. Apply the exact checked-in sanitize block with the marker bound
  // as SYNTHETIC_NNTP_PASS (as CI binds the real secret) and assert the
  // marker is absent from every file left under the upload path, including
  // base64-decoded payloads.
  test('real failing fill leaves no marker after the checked-in sanitize', async () => {
    test.setTimeout(120_000);
    const script = sanitizeBlocks[0]!.replace(/^ {10}/gm, '');
    const marker = 'MARKER-NNTP-PASS-9f8e7d6c5b4a';
    // Probe lives under e2e/ so `import '@playwright/test'` in the generated
    // spec resolves via e2e/node_modules (a system-temp dir has no ancestors
    // with the package). Removed in `finally`.
    const e2eDir = fileURLToPath(new URL('..', import.meta.url));
    const probe = mkdtempSync(join(e2eDir, '.real-sanitize-'));
    try {
      writeFileSync(
        join(probe, 'marker.spec.ts'),
        `import { test, expect } from '@playwright/test';\n` +
          `test('fills marker password then fails', async ({ page }) => {\n` +
          `  await page.setContent('<input data-testid="byop-pass" type="password" />');\n` +
          `  await page.getByTestId('byop-pass').fill('${marker}');\n` +
          // Fail on a page assertion so the failure snapshots the filled page.
          `  await expect(page.getByTestId('byop-pass')).toHaveValue('definitely-not-the-marker');\n` +
          `});\n`,
      );
      writeFileSync(
        join(probe, 'playwright.config.ts'),
        `import { defineConfig } from '@playwright/test';\n` +
          `export default defineConfig({\n` +
          `  testDir: '.',\n` +
          `  testMatch: 'marker.spec.ts',\n` +
          `  timeout: 30_000,\n` +
          `  retries: 0,\n` +
          `  reporter: [['list'], ['junit', { outputFile: 'test-results/junit.xml' }]],\n` +
          `  use: { trace: 'off', screenshot: 'off' },\n` +
          `  outputDir: 'test-results',\n` +
          `});\n`,
      );
      const cli = join(e2eDir, 'node_modules', '@playwright', 'test', 'cli.js');
      const run = spawnSync(process.execPath, [cli, 'test', `--config=${join(probe, 'playwright.config.ts')}`], {
        encoding: 'utf8',
        timeout: 90_000,
        cwd: probe,
        env: { ...process.env, CI: '1' },
      });
      const probeOutput = `${run.stdout}\n${run.stderr}`;
      // Minimal containers (no system browser libs) cannot launch Chromium;
      // CI installs --with-deps so the probe runs for real there. Skip here
      // rather than failing the suite on environment, never on assertion.
      if (/cannot open shared object|browser has been closed|browserType\.launch|executable doesn't exist/i.test(probeOutput)) {
        test.skip(true, 'Chromium system libraries unavailable in this container — probe runs in CI with --with-deps.');
        return;
      }
      // The probe must actually fail (otherwise it proved nothing).
      expect(run.status, `probe run output:\n${probeOutput}`).not.toBe(0);
      // The failing run must have produced the leak-prone snapshot with the
      // marker (otherwise the sanitize assertion is vacuous).
      const errorContexts = listFilesRecursive(probe).filter((f) => f.endsWith('error-context.md'));
      expect(errorContexts.length).toBeGreaterThan(0);
      expect(errorContexts.some((f) => readFileSync(f, 'utf8').includes(marker))).toBe(true);
      // junit.xml embeds the failure message (Received string), so it DOES
      // carry the marker before sanitize — this is the leak the scrub fixes.
      const junitBefore = listFilesRecursive(probe).filter((f) => f.endsWith('.xml'));
      expect(junitBefore.length).toBeGreaterThan(0);
      expect(junitBefore.some((f) => readFileSync(f, 'utf8').includes(marker))).toBe(true);

      // CI binds the real secret to the sanitize step; the probe binds the
      // marker the same way so the exact checked-in scrub removes it.
      const sanitized = runShell(script, { SYNTHETIC_NNTP_PASS: marker }, probe);
      expect(sanitized.status).toBe(0);
      expect(listFilesRecursive(probe).filter((f) => f.endsWith('error-context.md'))).toHaveLength(0);
      expect(existsSync(join(probe, 'playwright-report'))).toBe(false);
      // Marker absent from every remaining file under the upload path,
      // including inside base64 payloads.
      for (const file of listFilesRecursive(join(probe, 'test-results'))) {
        expect(fileContainsMarker(file, marker), file).toBe(false);
      }
      // Non-credential evidence survives sanitization.
      expect(existsSync(join(probe, 'test-results', 'junit.xml'))).toBe(true);
    } finally {
      rmSync(probe, { recursive: true, force: true });
    }
  });
});

// Fail-closed publication gate (acceptance contract for the Builder):
// Today both upload steps run on `failure() && configured` alone and the
// sanitize blocks swallow cleanup failures (`|| true`, stderr discarded)
// and per-file read errors (`except OSError: continue`), so a sanitizer
// that failed, was skipped, or silently left a file unscrubbed still
// publishes. The contract pinned below:
//   1. Each sanitize step carries an `id:` and its job's upload step adds
//      `steps.<sanitize-id>.outcome == 'success'` to its existing
//      `failure() && steps.staging.outputs.configured == 'true'`
//      condition. Skipped / failed / cancelled sanitize => no upload.
//   2. The sanitize blocks exit nonzero (names-only reporting, never file
//      contents) on any read or write failure of a file under the upload
//      path, instead of leaving it eligible.
//   3. A sanitize run may only report success when no `error-context.md`
//      path remains under test-results/ and playwright-report/ is gone:
//      a cleanup that could not remove them must fail, not pass silently.
//   4. A legitimately absent test-results/ is handled explicitly: success
//      (nothing is eligible) with an output notice stating it was absent,
//      not a generic "sanitized" claim and not a crash/traceback.
//   5. Uploads stay junit-only test-results/ with 7-day retention.
// The tests execute the exact checked-in sanitize blocks (extracted
// above) against synthetic fixtures. Failure injection is deterministic
// and root-proof (no chmod): /proc/self/mem reads raise EIO and
// /proc/version writes raise EACCES even for root, and a non-empty
// directory named error-context.md cannot be removed by file deletion.
const uploadSections = [...stagingWorkflow.matchAll(
  /      - name: Upload failure artifacts\n        if: ([^\n]+)\n([\s\S]*?)retention-days: (\d+)/g,
)];

function evaluateUploadGate(
  condition: string,
  scenario: { jobFailed: boolean; configured: boolean; sanitizeOutcome: string },
): boolean {
  const expr = condition
    .replace(/failure\(\)/g, String(scenario.jobFailed))
    .replace(
      /steps\.staging\.outputs\.configured\s*==\s*'true'/g,
      String(scenario.configured),
    )
    .replace(
      /steps\.([A-Za-z0-9_-]+)\.outcome\s*==\s*'success'/g,
      String(scenario.sanitizeOutcome === 'success'),
    );
  // Anything the evaluator does not understand stays visible as a failure
  // here rather than being silently treated as either true or false.
  expect(expr, `unmodelled upload-gate syntax in: ${condition}`).toMatch(
    /^[\s()!&|truefals]+$/,
  );
  return Function(`"use strict"; return (${expr});`)() as boolean;
}

function makeProbe(): string {
  return mkdtempSync(
    join(process.env.PAPERCLIP_RUN_SCRATCH_DIR || tmpdir(), 'artifact-gate-'),
  );
}

const GATE_MARKER = 'SENTINEL-SECRET-VALUE-GATE';

test.describe('fail-closed artifact publication gate', () => {
  test('both uploads require a successful sanitizer outcome (evaluated)', () => {
    expect(uploadSections).toHaveLength(2);
    expect(sanitizeSections).toHaveLength(2);
    for (const [index, upload] of uploadSections.entries()) {
      const condition = upload[1]!;
      const sanitizeId = sanitizeSections[index]![1]!.match(/^\s+id:\s*(\S+)/m)?.[1];
      expect(
        sanitizeId,
        `sanitize step ${index} must carry an id so the upload can gate on its outcome`,
      ).toBeDefined();
      expect(condition).toContain('failure()');
      expect(condition).toContain(`steps.staging.outputs.configured == 'true'`);
      expect(condition).toContain(`steps.${sanitizeId!}.outcome == 'success'`);

      const base = { jobFailed: true, configured: true };
      expect(evaluateUploadGate(condition, { ...base, sanitizeOutcome: 'success' })).toBe(true);
      for (const outcome of ['failure', 'skipped', 'cancelled']) {
        expect(
          evaluateUploadGate(condition, { ...base, sanitizeOutcome: outcome }),
          `upload ${index} must not publish when sanitize outcome is ${outcome}`,
        ).toBe(false);
      }
      expect(evaluateUploadGate(condition, { jobFailed: false, configured: true, sanitizeOutcome: 'success' })).toBe(false);
      expect(evaluateUploadGate(condition, { jobFailed: true, configured: false, sanitizeOutcome: 'success' })).toBe(false);
    }
  });

  test('uploads keep junit-only path and 7-day retention', () => {
    expect(uploadSections).toHaveLength(2);
    for (const upload of uploadSections) {
      expect(upload[2]).toContain('e2e/test-results/');
      expect(upload[2]).not.toContain('playwright-report');
      expect(upload[2]).not.toContain('trace');
      expect(upload[2]).not.toContain('screenshot');
      expect(upload[3]).toBe('7');
    }
  });

  for (const [index, block] of sanitizeBlocks.entries()) {
    const script = block.replace(/^ {10}/gm, '');
    const job = index === 0 ? 'synthetic' : 'journey';

    test(`${job} sanitizer: positive control publishes only scrubbed junit`, () => {
      const probe = makeProbe();
      try {
        mkdirSync(join(probe, 'test-results', 'case-1'), { recursive: true });
        mkdirSync(join(probe, 'playwright-report'), { recursive: true });
        writeFileSync(join(probe, 'test-results', 'junit.xml'), `<x>${GATE_MARKER}</x>`);
        writeFileSync(join(probe, 'test-results', 'case-1', 'error-context.md'), GATE_MARKER);
        writeFileSync(join(probe, 'playwright-report', 'index.html'), GATE_MARKER);
        const result = runShell(script, { SYNTHETIC_NNTP_PASS: GATE_MARKER }, probe);
        expect(result.status).toBe(0);
        expect(result.output).not.toContain(GATE_MARKER);
        expect(existsSync(join(probe, 'playwright-report'))).toBe(false);
        const remaining = listFilesRecursive(join(probe, 'test-results'));
        expect(remaining.some((f) => f.endsWith('error-context.md'))).toBe(false);
        for (const file of remaining) {
          expect(fileContainsMarker(file, GATE_MARKER), file).toBe(false);
        }
        expect(readFileSync(join(probe, 'test-results', 'junit.xml'), 'utf8')).toContain('***REDACTED***');
      } finally {
        rmSync(probe, { recursive: true, force: true });
      }
    });

    test(`${job} sanitizer: unreadable file fails closed without leaking contents`, () => {
      const probe = makeProbe();
      try {
        mkdirSync(join(probe, 'test-results'), { recursive: true });
        writeFileSync(join(probe, 'test-results', 'junit.xml'), `<x>${GATE_MARKER}</x>`);
        // Regular file whose reads deterministically raise EIO (root too).
        symlinkSync('/proc/self/mem', join(probe, 'test-results', 'unreadable.xml'));
        const result = runShell(script, { SYNTHETIC_NNTP_PASS: GATE_MARKER }, probe);
        expect(
          result.status,
          'a file the scrub could not read must fail the sanitizer, not stay upload-eligible',
        ).not.toBe(0);
        expect(result.output).not.toContain(GATE_MARKER);
      } finally {
        rmSync(probe, { recursive: true, force: true });
      }
    });

    test(`${job} sanitizer: unwritable file fails closed without leaking contents`, () => {
      const probe = makeProbe();
      try {
        mkdirSync(join(probe, 'test-results'), { recursive: true });
        // Readable regular file whose writes deterministically raise EACCES.
        symlinkSync('/proc/version', join(probe, 'test-results', 'junit.xml'));
        const result = runShell(script, { SYNTHETIC_NNTP_PASS: GATE_MARKER }, probe);
        expect(result.status).not.toBe(0);
        expect(result.output).not.toContain(GATE_MARKER);
      } finally {
        rmSync(probe, { recursive: true, force: true });
      }
    });

    test(`${job} sanitizer: undeletable error-context cannot report success`, () => {
      const probe = makeProbe();
      try {
        // A non-empty DIRECTORY named error-context.md: file deletion
        // (`find -delete`, unlink) fails deterministically even for root.
        // Success is acceptable only if the path is actually gone
        // afterwards (a recursive remove is a legitimate stronger cleanup).
        const ctxDir = join(probe, 'test-results', 'case-1', 'error-context.md');
        mkdirSync(ctxDir, { recursive: true });
        writeFileSync(join(ctxDir, 'inner.txt'), GATE_MARKER);
        writeFileSync(join(probe, 'test-results', 'junit.xml'), '<x>ok</x>');
        const result = runShell(script, { SYNTHETIC_NNTP_PASS: GATE_MARKER }, probe);
        if (result.status === 0) {
          expect(
            existsSync(ctxDir),
            'sanitizer reported success while error-context.md remains upload-eligible',
          ).toBe(false);
        }
        expect(result.output).not.toContain(GATE_MARKER);
      } finally {
        rmSync(probe, { recursive: true, force: true });
      }
    });

    test(`${job} sanitizer: absent test-results is explicit, not a generic success claim`, () => {
      const probe = makeProbe();
      try {
        const result = runShell(script, { SYNTHETIC_NNTP_PASS: GATE_MARKER }, probe);
        expect(result.status).toBe(0);
        expect(result.output).toMatch(
          /absent|missing|no test-results|nothing to (sanitize|upload)/i,
        );
        expect(result.output).not.toContain(GATE_MARKER);
        expect(existsSync(join(probe, 'test-results'))).toBe(false);
      } finally {
        rmSync(probe, { recursive: true, force: true });
      }
    });
  }
});

// Post-deploy sequencing contract: the full journey is promotion evidence,
// so it may run only after the same run's staging-web deploy succeeds.
// e2e-staging.yml must not race deploy.yml on push; instead deploy.yml
// calls it as a reusable workflow from a job that `needs` staging-web,
// and the journey job additionally requires the caller-only
// `inputs.post_deploy`. Scheduled/manual runs stay synthetic monitoring.
const deployWorkflow = readFileSync(new URL('deploy.yml', workflowDir), 'utf8');

function jobSection(workflow: string, job: string): string {
  const marker = `\n  ${job}:\n`;
  const start = workflow.indexOf(marker);
  expect(start, `job ${job} not found`).toBeGreaterThan(-1);
  const bodyStart = start + marker.length;
  const next = /\n  [A-Za-z0-9_-]+:\n/.exec(workflow.slice(bodyStart));
  return workflow.slice(bodyStart, next ? bodyStart + next.index : undefined);
}

function jobIf(workflow: string, job: string): string | undefined {
  // Job-level keys sit at 4-space indent; step conditions are deeper.
  return jobSection(workflow, job).match(/^    if: (.+)$/m)?.[1];
}

const stagingOnBlock = stagingWorkflow
  .split('\non:\n')[1]!
  .split('\npermissions:\n')[0]!;

test.describe('post-deploy sequencing (deploy → full journey)', () => {
  test('e2e-staging does not trigger on push (no deploy race)', () => {
    expect(stagingOnBlock).not.toMatch(/^  push:/m);
    expect(stagingOnBlock).toMatch(/^  workflow_call:/m);
    // Manual runs exist but define no inputs, so a dispatch cannot set
    // post_deploy and mint promotion evidence by hand.
    expect(stagingOnBlock).toContain('  workflow_dispatch:\n  schedule:');
    expect(stagingOnBlock).toContain('- cron: "17 */6 * * *"');
  });

  test('post_deploy is a required boolean workflow_call input', () => {
    const callBlock = stagingOnBlock
      .split('  workflow_call:\n')[1]!
      .split('\n  workflow_dispatch:')[0]!;
    expect(callBlock).toMatch(/ {6}post_deploy:\n(?:.*\n)*? {8}type: boolean\n {8}required: true/);
  });

  test('full journey runs only for the post-deploy call (evaluated)', () => {
    const condition = jobIf(stagingWorkflow, 'e2e');
    expect(condition, 'e2e job must carry a job-level if').toBeDefined();
    expect(condition).not.toContain('always()');
    expect(condition).not.toContain('!cancelled()');
    expect(jobSection(stagingWorkflow, 'e2e')).toContain('needs: [synthetic]');

    const evaluate = (postDeploy: boolean | ''): boolean => {
      const inner = condition!
        .replace(/^\$\{\{\s*/, '')
        .replace(/\s*\}\}$/, '');
      const expr = inner.replace(
        /inputs\.post_deploy/g,
        postDeploy === '' ? 'false' : String(postDeploy),
      );
      expect(expr, `unmodelled journey-gate syntax in: ${condition}`).toMatch(
        /^[\s()!&|truefals]+$/,
      );
      return Function(`"use strict"; return (${expr});`)() as boolean;
    };
    // workflow_call from deploy.yml passes post_deploy: true.
    expect(evaluate(true)).toBe(true);
    // workflow_dispatch/schedule leave the input unset: monitoring only.
    expect(evaluate('')).toBe(false);
    expect(evaluate(false)).toBe(false);
  });

  test('synthetic smoke stays ungated monitoring on every trigger', () => {
    // No job-level if: scheduled and manual runs execute the smoke (and
    // step-level skip while staging is unconfigured), distinct from the
    // gated full journey above.
    expect(jobIf(stagingWorkflow, 'synthetic')).toBeUndefined();
  });

  test('deploy calls e2e-staging only after a successful push staging-web (evaluated)', () => {
    // The deploy being sequenced after is itself push-only and gated on
    // the Cloudflare token; when it skips, dependents must skip too.
    expect(jobIf(deployWorkflow, 'staging-web')).toContain("github.event_name == 'push'");
    expect(jobIf(deployWorkflow, 'staging-web')).toContain("needs.gate.outputs.enabled == 'true'");

    const section = jobSection(deployWorkflow, 'post-deploy-e2e');
    expect(section).toContain('needs: [staging-web]');
    expect(section).toContain('uses: ./.github/workflows/e2e-staging.yml');
    expect(section).toMatch(/post_deploy: true/);

    const condition = jobIf(deployWorkflow, 'post-deploy-e2e');
    expect(condition, 'post-deploy-e2e must carry a job-level if').toBeDefined();
    // No status-check function in the condition: GitHub then applies the
    // implicit success() over `needs`, which is the whole contract — a
    // skipped/failed/cancelled staging-web can never run this job.
    for (const statusFn of ['always()', '!cancelled()', 'success()', 'failure()', 'cancelled()']) {
      expect(condition, `post-deploy-e2e if must not use ${statusFn}`).not.toContain(statusFn);
    }

    const evaluate = (stagingWebResult: string, eventName: string): boolean => {
      const inner = condition!
        .replace(/^\$\{\{\s*/, '')
        .replace(/\s*\}\}$/, '');
      const expr = inner.replace(
        /github\.event_name\s*==\s*'push'/g,
        String(eventName === 'push'),
      );
      expect(expr, `unmodelled post-deploy syntax in: ${condition}`).toMatch(
        /^[\s()!&|truefals]+$/,
      );
      const conditionHolds = Function(`"use strict"; return (${expr});`)() as boolean;
      // Implicit success() over needs.
      return stagingWebResult === 'success' && conditionHolds;
    };
    expect(evaluate('success', 'push')).toBe(true);
    for (const result of ['skipped', 'failure', 'cancelled']) {
      expect(
        evaluate(result, 'push'),
        `staging-web ${result} must not produce a full-journey run`,
      ).toBe(false);
    }
    for (const event of ['pull_request', 'workflow_dispatch', 'schedule']) {
      expect(evaluate('success', event), `${event} must not run post-deploy E2E`).toBe(false);
    }
  });

  test('deploy concurrency is job-level so the E2E tail never holds the deploy group', () => {
    // A workflow-level group is held until every job in the run finishes —
    // now including up to ~40 min of post-deploy E2E — so queued main
    // pushes would be cancelled before ever deploying. Job-level groups
    // release when the deploy finishes; E2E serializes on e2e-staging.
    expect(deployWorkflow.split('\njobs:\n')[0]!).not.toMatch(/^concurrency:/m);
    for (const job of ['staging-web', 'staging-api', 'preview-web']) {
      const section = jobSection(deployWorkflow, job);
      expect(section).toMatch(/^    concurrency:\n      group: .+\n      cancel-in-progress: /m);
      expect(section).toContain("cancel-in-progress: ${{ github.ref != 'refs/heads/main' }}");
    }
    // staging-api serializes on its own group: one pending job per group,
    // so sharing staging-web's group lets a push's own lanes cancel each
    // other and leaves web and api at different SHAs with no post-deploy
    // E2E. (preview-web shares the web expression but only runs on PR
    // events, so it never contends with the push-only lanes.)
    const webGroup = jobSection(deployWorkflow, 'staging-web').match(/^      group: (.+)$/m)![1];
    const apiGroup = jobSection(deployWorkflow, 'staging-api').match(/^      group: (.+)$/m)![1];
    expect(apiGroup).toContain('staging-api');
    expect(apiGroup, 'staging-api must not share the staging-web group').not.toBe(webGroup);
    // post-deploy-e2e needs no group of its own: the called workflow
    // serializes on its e2e-staging group.
    expect(jobSection(deployWorkflow, 'post-deploy-e2e')).not.toContain('concurrency:');
    expect(stagingWorkflow.split('\njobs:\n')[0]!).toMatch(
      /^concurrency:\n  group: e2e-staging\n  cancel-in-progress: false/m,
    );
  });

  test('deploy.yml is the only reusable-workflow caller of e2e-staging', () => {
    for (const name of readdirSync(fileURLToPath(workflowDir)).filter(n => n.endsWith('.yml'))) {
      const workflow = readFileSync(new URL(name, workflowDir), 'utf8');
      const calls = workflow.split('uses: ./.github/workflows/e2e-staging.yml').length - 1;
      expect(calls, `${name} calls e2e-staging.yml`).toBe(name === 'deploy.yml' ? 1 : 0);
    }
  });

  test('caller passes exactly the secrets e2e-staging reads, explicitly', () => {
    expect(deployWorkflow).not.toContain('secrets: inherit');
    const used = [...new Set(
      [...stagingWorkflow.matchAll(/secrets\.([A-Z0-9_]+)/g)].map(m => m[1]!),
    )].sort();
    const declaredBlock = stagingOnBlock
      .split('    secrets:\n')[1]!
      .split('\n  workflow_dispatch:')[0]!;
    const declared = [...declaredBlock.matchAll(/^      ([A-Z0-9_]+):$/gm)]
      .map(m => m[1]!)
      .sort();
    const passedBlock = jobSection(deployWorkflow, 'post-deploy-e2e').split('    secrets:\n')[1]!;
    const passed = [...passedBlock.matchAll(/^      ([A-Z0-9_]+): \$\{\{ secrets\.([A-Z0-9_]+) \}\}$/gm)];
    expect(declared, 'workflow_call secret declarations').toEqual(used);
    expect(passed.map(m => m[1]!).sort(), 'secrets passed by deploy.yml').toEqual(used);
    for (const m of passed) expect(m[2], 'secret passed under its own name').toBe(m[1]);
    // Every declared secret stays optional so the pre-configuration
    // step-level skip (staging not configured yet) keeps working.
    for (const m of declaredBlock.matchAll(/required: (\w+)/g)) {
      expect(m[1]).toBe('false');
    }
  });

  test('least permissions and no production deploy path are preserved', () => {
    for (const workflow of [stagingWorkflow, deployWorkflow]) {
      expect(workflow).toContain('\npermissions:\n  contents: read\n');
    }
    expect(stagingWorkflow).not.toMatch(/:\s*write\b/);
    const section = jobSection(deployWorkflow, 'post-deploy-e2e');
    expect(section).toContain('    permissions:\n      contents: read\n');
    expect(section).not.toMatch(/:\s*write\b/);
    expect(stagingWorkflow).not.toContain('wrangler');
    expect(stagingWorkflow).not.toContain('--env production');
    expect(section).not.toContain('deploy --env');
  });
});

// Locked-dependency coverage builds (static pin): coverage.yml builds
// `--locked` for every standalone crate with a committed workers/<crate>/
// Cargo.lock plus the root workspace (covering stream), and fails closed on
// missing reports — but nothing stops a future edit from dropping a
// `--locked` flag or adding an uncovered standalone crate. These cases pin
// the mapping offline so such an edit reds test:unit.
// Commented-out steps are inactive: strip full-line `#` comments once at load
// so a commented-out llvm-cov command cannot satisfy the pins below.
const coverageWorkflow = readFileSync(new URL('coverage.yml', workflowDir), 'utf8').replace(
  /^[ \t]*#.*$/gm,
  '',
);

// Join backslash continuations so multi-line invocations read as one command.
const coverageCommands = coverageWorkflow
  .replace(/\\\n/g, '')
  .split('\n')
  .filter((line) => line.includes('cargo llvm-cov'));

// `for crate in ingest mb; do cargo llvm-cov --locked --manifest-path
// "workers/${crate}/Cargo.toml" ...` covers a set of crates with one generic
// locked step: resolve the loop word lists whose OWN body is such a step.
// The body scope matters: the summary loop below (`for crate in root api
// ingest mb repair`) never runs llvm-cov, so testing the whole file would
// wrongly credit its word list.
function loopCoveredCrates(): Set<string> {
  const covered = new Set<string>();
  for (const match of coverageWorkflow.matchAll(/for (\w+) in ([^;]+); do([\s\S]*?)done/g)) {
    const variable = match[1]!;
    const words = match[2]!.trim().split(/\s+/);
    const body = match[3]!;
    const generic = new RegExp(
      `cargo llvm-cov --locked --manifest-path\\s+"?workers/\\$\\{${variable}\\}/Cargo\\.toml"?`,
    );
    if (generic.test(body)) {
      for (const word of words) covered.add(word);
    }
  }
  return covered;
}

function crateHasLockedCoverageStep(crate: string, loopCovered: Set<string>): boolean {
  // Accept the manifest path quoted or unquoted: both are valid shell.
  const direct = new RegExp(
    `cargo llvm-cov --locked --manifest-path\\s+"?workers/${crate}/Cargo\\.toml"?`,
  );
  return direct.test(coverageWorkflow) || loopCovered.has(crate);
}

test.describe('locked-dependency coverage builds (static pin)', () => {
  test('every committed workers lockfile has a --locked llvm-cov step', () => {
    // Enumerated from the tree (not a hardcoded list) so a newly added
    // standalone crate with a committed Cargo.lock fails until coverage.yml
    // gains a locked step for it.
    const workersDir = fileURLToPath(new URL('../../workers/', import.meta.url));
    const lockCrates = readdirSync(workersDir, { withFileTypes: true })
      .filter((entry) => entry.isDirectory())
      .map((entry) => entry.name)
      .filter((name) => existsSync(join(workersDir, name, 'Cargo.lock')))
      .sort();
    expect(lockCrates.length).toBeGreaterThan(0);
    const loopCovered = loopCoveredCrates();
    for (const crate of lockCrates) {
      expect(
        crateHasLockedCoverageStep(crate, loopCovered),
        `workers/${crate}/Cargo.lock has no --locked llvm-cov step in coverage.yml`,
      ).toBe(true);
    }
  });

  test('no manifest-pinned llvm-cov step drops its --locked flag', () => {
    expect(coverageCommands.length).toBeGreaterThan(0);
    for (const command of coverageCommands) {
      if (command.includes('--manifest-path')) {
        expect(command, `llvm-cov step lost its --locked flag: ${command.trim()}`).toContain(
          '--locked',
        );
      }
    }
  });

  test('stream stays covered by the root workspace step while a member, locked step if extracted', () => {
    const rootManifest = readFileSync(new URL('../../Cargo.toml', import.meta.url), 'utf8');
    const members = rootManifest.match(/members = \[([^\]]*)\]/)?.[1] ?? '';
    // Compare complete member values: a substring match would let e.g.
    // `workers/stream-tools` select the workspace branch and bypass the
    // locked stream-step assertion.
    const memberValues = [...members.matchAll(/"([^"]+)"/g)].map((m) => m[1]);
    if (memberValues.includes('workers/stream')) {
      expect(
        coverageCommands.some(
          (command) =>
            command.includes('--workspace') &&
            !command.includes('--manifest-path') &&
            !/--exclude|--package|\s-p\s/.test(command),
        ),
        'root workspace llvm-cov step covering workers/stream is missing',
      ).toBe(true);
    } else {
      expect(
        crateHasLockedCoverageStep('stream', loopCoveredCrates()),
        'workers/stream left the root workspace without a --locked llvm-cov step',
      ).toBe(true);
    }
  });
});

test.describe('deploy.yml backend Worker lanes stay evidenced (api staging only)', () => {
  // deploy.yml deploys apps/web plus the one authorized backend lane,
  // `staging-api` (api Worker, staging only); every further backend lane
  // (ingest/mb/stream/repair) must extend this guard with its own
  // allowlist + evidence instead of weakening it. These pins fail on any
  // new `deploy-api-worker`-style job with `wrangler deploy --env staging`,
  // anything deploying under workers/ outside `staging-api`, any
  // production-environment deploy step, or a bare `command: deploy`
  // (apps/web top-level name equals the production env name, so a bare
  // deploy targets production).
  const jobsBody = deployWorkflow.split('\njobs:\n')[1]!;

  // The one authorized backend lane. Everything outside it stays web-only.
  const bodyWithoutJob = (body: string, job: string): string => {
    const section = jobSection(body, job);
    return body.split(section).join('');
  };
  const codeLines = (text: string): string =>
    text
      .split('\n')
      .filter((l) => !/^\s*#/.test(l))
      .join('\n');

  // Shared guard: the real pins below call these helpers, and the mutant
  // test asserts the same helpers throw for each synthetic backend lane —
  // so weakening a pin breaks the mutant test instead of silently passing.
  const assertJobSet = (body: string) => {
    const names = [...body.matchAll(/^  ([A-Za-z0-9_-]+):$/gm)].map((m) => m[1]!).sort();
    expect(names).toEqual(['gate', 'post-deploy-e2e', 'preview-web', 'staging-api', 'staging-web']);
    for (const name of names) {
      expect(name, `job ${name} must not look like a backend deploy lane`).not.toMatch(
        /worker|backend/i,
      );
    }
  };

  const assertDeployBody = (body: string) => {
    expect(body.match(/wrangler-action/g) ?? []).toHaveLength(3);
    expect(body.match(/deploy --env/g) ?? []).toHaveLength(2);
    // Exact command pin: staging-web deploys to staging, staging-api
    // deploys the api Worker to staging with the public BUILD_SHA var, and
    // preview-web runs `preview`. A bare `command: deploy` (no --env)
    // deploys the top-level worker name, which equals the production env
    // name — count checks alone miss it, so pin every wrangler-action
    // command value exactly.
    const commands = [...body.matchAll(/^\s+command: (.+)$/gm)]
      .map((m) => m[1]!.trim())
      .sort();
    expect(
      commands,
      `wrangler commands stay exactly the evidenced lanes (got ${JSON.stringify(commands)})`,
    ).toEqual([
      'deploy --env staging',
      'deploy --env staging --var BUILD_SHA:${{ github.sha }}',
      'preview',
    ]);
    // Raw-CLI pin: no `run:` step may invoke a wrangler deploy, even split
    // across lines (`wrangler \` + newline + `deploy`), version-pinned
    // (`wrangler@<ver> deploy`), or flag-first (`wrangler --config x
    // deploy`). Join continuations, then match `wrangler` (excluding
    // `wrangler-action` and `wranglerVersion`) followed anywhere on the
    // same logical line by `deploy`.
    const flat = body.replace(/\\\r?\n\s*/g, ' ');
    expect(flat).not.toMatch(/\bwrangler\b(?!-action)[^\n]*\bdeploy\b/);
    // Working directories: apps/web everywhere except the api lane, which
    // is the only job allowed to touch workers/api.
    for (const line of bodyWithoutJob(body, 'staging-api').split('\n')) {
      if (line.includes('workingDirectory:') || line.includes('working-directory:')) {
        expect(line, `deploy working directory stays apps/web (${line.trim()})`).toContain(
          'apps/web',
        );
      }
    }
    for (const line of jobSection(body, 'staging-api').split('\n')) {
      if (line.includes('workingDirectory:') || line.includes('working-directory:')) {
        expect(line, `api lane deploys from workers/api (${line.trim()})`).toContain(
          'workers/api',
        );
      }
    }
  };

  const assertNoBackendPath = (body: string) => {
    // Header comments (above `jobs:`) legitimately name workers/ and the
    // Hyperdrive gate; outside `staging-api` the jobs body itself stays
    // web-only with no production path. Case-sensitive: the preview job
    // carries a `Workers preview deployment` comment (no path), which must
    // keep passing.
    // Production pin: no job carries the `production` word at all in code
    // (comments stripped) — that covers `--env production`,
    // `--env=production`, `environment: production`, the mapping form
    // (`environment:\n  name: production`), quoted values, and
    // `CLOUDFLARE_ENV: production` — while a harmless full-line comment
    // mentioning production still passes. The api lane is staging-only, so
    // the pin applies to its section too.
    const rest = bodyWithoutJob(body, 'staging-api');
    expect(rest).not.toContain('workers/');
    expect(codeLines(rest).toLowerCase()).not.toContain('production');
    expect(codeLines(jobSection(body, 'staging-api')).toLowerCase()).not.toContain('production');
  };

  const assertStagingApi = (body: string) => {
    // Evidence the api staging lane must keep: push-only + token gate,
    // staging-only deploy with the public BUILD_SHA var, the fail-closed
    // Hyperdrive cache-disabled gate (CI-only --allow-skip forbidden), and
    // no GitHub-environment mapping (which would resolve prod-scoped
    // secrets and protection rules).
    const section = jobSection(body, 'staging-api');
    expect(section).toContain('needs: [gate]');
    const condition = jobIf(body, 'staging-api');
    expect(condition, 'staging-api must carry a job-level if').toBeDefined();
    expect(condition).toContain("github.event_name == 'push'");
    expect(condition).toContain("needs.gate.outputs.enabled == 'true'");
    expect(section).toContain('command: deploy --env staging --var BUILD_SHA:${{ github.sha }}');
    expect(section).toContain('check_hyperdrive_cache.sh');
    expect(section, 'real deploys must not pass --allow-skip').not.toContain('--allow-skip');
    expect(section, 'api lane must not map a GitHub environment').not.toMatch(/^\s*environment:/m);
    // Own concurrency group: sharing staging-web's group lets a push's
    // own lanes cancel each other (one pending job per group).
    expect(section).toContain('group: ${{ github.workflow }}-staging-api-${{ github.ref }}');
    // Generated-config parity: the only real api wrangler config is
    // generated from HYPERDRIVE_ID_STAGING, which binding-parity never
    // reads — the lane pins it to the committed staging inventory id.
    expect(section).toContain('binding_manifest.toml');
    expect(section).toContain('HYPERDRIVE_ID_STAGING must equal the api staging inventory id');
    // Workers Logs persistence: the generated staging config must enable
    // observability at full head sampling, otherwise the deployed Worker
    // persists no records (or a sampled subset) and the build_sha log
    // evidence can never be read back reliably.
    expect(section).toMatch(
      /^[ \t]*\[env\.staging\.observability\]\n[ \t]+enabled = true\n[ \t]+head_sampling_rate = 1\n/m,
    );
    // Public vars parity: the generated staging config must carry all four
    // public [env.staging.vars] lines (RP pair from repo vars, session TTL
    // literal, cache-disabled attestation). A dropped SESSION_TTL_SECS falls
    // back to a compiled default and a dropped HYPERDRIVE_CACHING_DISABLED
    // refuses traffic — both merge green without this pin. Names plus
    // ${...}/literal shape only, never secret values.
    expect(section).toMatch(
      /^[ \t]*\[env\.staging\.vars\]\n[ \t]+RP_ID = "\$\{API_RP_ID_STAGING\}"\n[ \t]+RP_ORIGIN = "\$\{API_RP_ORIGIN_STAGING\}"\n[ \t]+SESSION_TTL_SECS = "86400"\n[ \t]+HYPERDRIVE_CACHING_DISABLED = "1"\n/m,
    );
  };

  const assertDeployGuard = (body: string) => {
    assertJobSet(body);
    assertDeployBody(body);
    assertNoBackendPath(body);
    assertStagingApi(body);
  };

  test('job set is exactly the evidenced lanes (web + api staging)', () => {
    assertJobSet(jobsBody);
  });

  test('only the evidenced staging steps run a wrangler deploy', () => {
    // staging-web deploys apps/web to staging, staging-api deploys the api
    // Worker to staging with BUILD_SHA, preview-web runs `preview` (not
    // deploy). Any additional wrangler deploy — action or raw `wrangler
    // deploy` CLI — is a backend-lane candidate until evidenced.
    assertDeployBody(jobsBody);
    expect(jobSection(deployWorkflow, 'staging-web')).toContain('command: deploy --env staging');
  });

  test('no workers/ path or production-environment deploy step outside staging-api', () => {
    assertNoBackendPath(jobsBody);
  });

  test('staging-api lane carries BUILD_SHA + hyperdrive gate evidence', () => {
    assertStagingApi(jobsBody);
  });

  test('synthetic backend-deploy mutants trip the pins above', () => {
    // Regression proof for the shared guard: each mutant models an
    // unauthorized backend lane and must throw the same helpers the real
    // pins call (not a restated copy of the expectations).
    const workerJob =
      '\n  deploy-api-worker:\n    runs-on: ubuntu-latest\n    defaults:\n      run:\n        working-directory: workers/api\n    steps:\n      - run: wrangler deploy --env staging\n';
    const workerMutant = `${jobsBody}${workerJob}`;
    const prodMutant = `${jobsBody}\n      - run: wrangler deploy --env production\n`;
    // Bare `command: deploy` on preview-web: no --env, no `wrangler deploy`
    // string, no workers/ or production marker — only the exact command pin
    // catches it, and it would deploy the production worker name.
    const bareDeployMutant = jobsBody.replace('command: preview', 'command: deploy');
    // Multiline raw-CLI deploy: `wrangler \` + newline + `deploy` evades a
    // contiguous `wrangler deploy` substring check but still deploys.
    const multilineMutant = `${jobsBody}\n      - run: pnpm exec wrangler \\\n        deploy --env staging\n`;
    // Version-pinned raw-CLI deploy (the workflow's own WRANGLER_VERSION
    // spelling): `@<ver>` sits between the tokens, so a
    // `wrangler\s+deploy` check misses it.
    const versionPinnedMutant = `${jobsBody}\n      - run: npx wrangler@\${{ env.WRANGLER_VERSION }} deploy\n`;
    // Flag-first raw-CLI deploy: a flag before the subcommand evades a
    // `wrangler deploy` adjacency check.
    const flagFirstMutant = `${jobsBody}\n      - run: pnpm exec wrangler --config wrangler.toml deploy\n`;
    // Production GitHub environment mapping form on a PR-triggered job:
    // resolves production-scoped secrets and protection rules.
    const envMappingMutant = jobsBody.replace(
      '    needs: [gate]\n    if: ${{ github.event_name == ',
      '    needs: [gate]\n    environment:\n      name: production\n    if: ${{ github.event_name == ',
    );
    // Quoted production environment value.
    const quotedEnvMutant = `${jobsBody}\n    environment: "production"\n`;
    // CI-only gate bypass in the api lane: the Hyperdrive cache-disabled
    // gate must never pass --allow-skip in a real deploy.
    const allowSkipMutant = jobsBody.replace(
      './check_hyperdrive_cache.sh --config-id "${{ vars.HYPERDRIVE_ID_STAGING }}"',
      './check_hyperdrive_cache.sh --config-id "${{ vars.HYPERDRIVE_ID_STAGING }}" --allow-skip',
    );
    // Api lane without the public BUILD_SHA var: records would render
    // "unknown" instead of the deploy SHA.
    const noBuildShaMutant = jobsBody.replace(' --var BUILD_SHA:${{ github.sha }}', '');
    // Api lane sharing staging-web's group: a push's own lanes cancel each
    // other, leaving web and api at different SHAs.
    const sharedGroupMutant = jobsBody.replace(
      'group: ${{ github.workflow }}-staging-api-${{ github.ref }}',
      'group: ${{ github.workflow }}-${{ github.event.pull_request.number || github.ref }}',
    );
    // Api lane without the generated-config parity pin: a mis-set
    // HYPERDRIVE_ID_STAGING (e.g. the prod value) deploys staging against
    // the wrong database with no CI failure.
    const noParityMutant = jobsBody.replace(
      'HYPERDRIVE_ID_STAGING must equal the api staging inventory id',
      'HYPERDRIVE_ID_STAGING looks fine',
    );
    // Api lane whose generated config drops the observability block: the
    // Worker deploys with observability=null and persists no Workers Logs.
    const noObservabilityMutant = jobsBody.replace('enabled = true', 'enabled = false');
    // Api lane whose generated config zeroes head sampling: the Worker keeps
    // the block but persists no records, so the build_sha evidence is unread.
    const zeroSamplingMutant = jobsBody.replace('head_sampling_rate = 1', 'head_sampling_rate = 0');
    // Api lane whose generated config drops the sampling line: the default
    // rate applies instead of the evidenced full sampling.
    const droppedSamplingMutant = jobsBody.replace('          head_sampling_rate = 1\n', '');
    // Api lane whose generated config comments out the observability header:
    // the TOML then defines no env.staging.observability table at all (the
    // two keys fall into env.staging.vars), and an unanchored pin would keep
    // matching the bracket text inside the comment.
    const commentedHeaderMutant = jobsBody.replace(
      '          [env.staging.observability]\n',
      '          # [env.staging.observability]\n',
    );
    // Generated staging vars mutants: dropping any one of the four public
    // vars lines must trip the pin above.
    const droppedRpIdMutant = jobsBody.replace('          RP_ID = "${API_RP_ID_STAGING}"\n', '');
    const droppedRpOriginMutant = jobsBody.replace(
      '          RP_ORIGIN = "${API_RP_ORIGIN_STAGING}"\n',
      '',
    );
    const droppedSessionTtlMutant = jobsBody.replace('          SESSION_TTL_SECS = "86400"\n', '');
    const droppedCachingDisabledMutant = jobsBody.replace(
      '          HYPERDRIVE_CACHING_DISABLED = "1"\n',
      '',
    );
    // Semantic vars mutants: each line stays present, so only the exact
    // literal / repo-var name / header anchor in the pin catches them — a
    // pin broadened to a generic quoted value or RP name, or one missing
    // its header anchor, would keep rejecting the drop mutants above yet
    // accept these.
    // Shorter session TTL literal than the evidenced one day.
    const shortSessionTtlMutant = jobsBody.replace(
      '          SESSION_TTL_SECS = "86400"\n',
      '          SESSION_TTL_SECS = "3600"\n',
    );
    // Cache-disabled attestation flipped off: the Worker would serve with
    // Hyperdrive caching enabled against the evidenced disabled posture.
    const cachingNotDisabledMutant = jobsBody.replace(
      '          HYPERDRIVE_CACHING_DISABLED = "1"\n',
      '          HYPERDRIVE_CACHING_DISABLED = "0"\n',
    );
    // RP_ID wired to the origin repo var (and the converse for RP_ORIGIN):
    // each exact repo-var name is protected independently.
    const rpIdSwappedMutant = jobsBody.replace(
      '          RP_ID = "${API_RP_ID_STAGING}"\n',
      '          RP_ID = "${API_RP_ORIGIN_STAGING}"\n',
    );
    const rpOriginSwappedMutant = jobsBody.replace(
      '          RP_ORIGIN = "${API_RP_ORIGIN_STAGING}"\n',
      '          RP_ORIGIN = "${API_RP_ID_STAGING}"\n',
    );
    // Commented vars header: the TOML then defines no env.staging.vars
    // table at all, and an unanchored pin would keep matching the bracket
    // text inside the comment.
    const commentedVarsHeaderMutant = jobsBody.replace(
      '          [env.staging.vars]\n',
      '          # [env.staging.vars]\n',
    );

    for (const mutant of [
      workerMutant,
      prodMutant,
      bareDeployMutant,
      multilineMutant,
      versionPinnedMutant,
      flagFirstMutant,
      envMappingMutant,
      quotedEnvMutant,
      allowSkipMutant,
      noBuildShaMutant,
      sharedGroupMutant,
      noParityMutant,
      noObservabilityMutant,
      zeroSamplingMutant,
      droppedSamplingMutant,
      commentedHeaderMutant,
      droppedRpIdMutant,
      droppedRpOriginMutant,
      droppedSessionTtlMutant,
      droppedCachingDisabledMutant,
      shortSessionTtlMutant,
      cachingNotDisabledMutant,
      rpIdSwappedMutant,
      rpOriginSwappedMutant,
      commentedVarsHeaderMutant,
    ]) {
      expect(() => assertDeployGuard(mutant), 'mutant must violate the deploy guard').toThrow();
    }
    // Targeted evidence for which pin each mutant trips.
    expect(() => assertJobSet(workerMutant)).toThrow();
    expect(() => assertDeployBody(workerMutant)).toThrow();
    expect(() => assertNoBackendPath(workerMutant)).toThrow();
    expect(() => assertDeployBody(prodMutant)).toThrow();
    expect(() => assertNoBackendPath(prodMutant)).toThrow();
    expect(() => assertDeployBody(bareDeployMutant)).toThrow();
    expect(() => assertDeployBody(multilineMutant)).toThrow();
    expect(() => assertDeployBody(versionPinnedMutant)).toThrow();
    expect(() => assertDeployBody(flagFirstMutant)).toThrow();
    expect(() => assertNoBackendPath(envMappingMutant)).toThrow();
    expect(() => assertNoBackendPath(quotedEnvMutant)).toThrow();
    expect(() => assertStagingApi(allowSkipMutant)).toThrow();
    expect(() => assertStagingApi(noBuildShaMutant)).toThrow();
    expect(() => assertDeployBody(noBuildShaMutant)).toThrow();
    expect(() => assertStagingApi(sharedGroupMutant)).toThrow();
    expect(() => assertStagingApi(noParityMutant)).toThrow();
    expect(() => assertStagingApi(noObservabilityMutant)).toThrow();
    expect(() => assertStagingApi(zeroSamplingMutant)).toThrow();
    expect(() => assertStagingApi(droppedSamplingMutant)).toThrow();
    expect(() => assertStagingApi(commentedHeaderMutant)).toThrow();
    expect(() => assertStagingApi(droppedRpIdMutant)).toThrow();
    expect(() => assertStagingApi(droppedRpOriginMutant)).toThrow();
    expect(() => assertStagingApi(droppedSessionTtlMutant)).toThrow();
    expect(() => assertStagingApi(droppedCachingDisabledMutant)).toThrow();
    expect(() => assertStagingApi(shortSessionTtlMutant)).toThrow();
    expect(() => assertStagingApi(cachingNotDisabledMutant)).toThrow();
    expect(() => assertStagingApi(rpIdSwappedMutant)).toThrow();
    expect(() => assertStagingApi(rpOriginSwappedMutant)).toThrow();
    expect(() => assertStagingApi(commentedVarsHeaderMutant)).toThrow();
  });
});

test.describe('deployed-build identity wiring (static pin)', () => {
  // Same-build evidence: the full journey proves the serving build is the
  // promoted one by comparing GET /api/build with the promotion SHA from the
  // push-triggered deploy run — never a source checkout. These pins keep the
  // chain (deploy expected_sha -> e2e-staging input -> EXPECTED_BUILD_SHA env
  // -> build-identity spec) honest offline.
  test('e2e-staging declares the expected_sha promotion input', () => {
    expect(stagingWorkflow).toContain('expected_sha:');
    expect(stagingWorkflow).toContain('Promotion SHA from the push-triggered deploy run');
  });

  test('deploy passes expected_sha from github.sha (never a checkout)', () => {
    const section = jobSection(deployWorkflow, 'post-deploy-e2e');
    expect(section).toContain('expected_sha: ${{ github.sha }}');
    expect(section).not.toContain('rev-parse');
  });

  test('the e2e job exposes EXPECTED_BUILD_SHA from inputs.expected_sha', () => {
    const section = jobSection(stagingWorkflow, 'e2e');
    expect(section).toContain('EXPECTED_BUILD_SHA: ${{ inputs.expected_sha }}');
  });

  test('contract carries the build identity path', () => {
    const contract = readFileSync(new URL('../helpers/contract.ts', import.meta.url), 'utf8');
    expect(contract).toContain("build: '/api/build'");
  });

  test('build-identity helper exposes the promotion/verdict contract', () => {
    const helper = readFileSync(
      new URL('../helpers/build-identity.ts', import.meta.url),
      'utf8',
    );
    expect(helper).toContain('export function expectedPromotionSha');
    expect(helper).toContain('export function extractDeployedSha');
    expect(helper).toContain('export function assertDeployedBuildMatchesPromotion');
    expect(helper).toContain('EXPECTED_BUILD_SHA');
    // Never a source checkout: the helper reads the run-provided env and
    // performs pure string comparison — no shell-out to git, no checkout read.
    // It never gates on staging itself (skip ownership stays with the spec).
    expect(helper).not.toContain('child_process');
    expect(helper).not.toContain('spawnSync');
    expect(helper).not.toContain('execSync');
    expect(helper).not.toContain('process.env.STAGING_BASE_URL');
    expect(helper).not.toContain('stagingBaseUrl(');
  });

  test('build-identity legs compare the serving build with the promotion SHA via the shared live helper', () => {
    // One-shot reads cannot bind a ~30 min journey: the opening leg proves
    // the deploy delivered the promoted build before the journey and the
    // closing leg re-reads after every other spec. Both route through one
    // shared helper so the two reads cannot drift apart.
    const helper = readFileSync(
      new URL('../helpers/build-identity-live.ts', import.meta.url),
      'utf8',
    );
    expect(helper).toContain('export async function assertServingBuildMatchesPromotion');
    expect(helper).toContain('API.build');
    expect(helper).toContain('expectedPromotionSha(');
    expect(helper).toContain('extractDeployedSha(');
    expect(helper).toContain('assertDeployedBuildMatchesPromotion(');
    expect(helper).toContain('assertNoAccessWall(');
    expect(helper).toContain('apiGet(');
    expect(helper).toContain('accessHeaders(');
    // The helper performs pure verdict throws, never a source checkout.
    expect(helper).not.toContain('rev-parse');
    expect(helper).not.toContain('child_process');

    const opener = readFileSync(
      new URL('../tests/build-identity.spec.ts', import.meta.url),
      'utf8',
    );
    expect(opener).toContain(
      'deployed build matches the promotion SHA (exact-head staging evidence)',
    );
    // Fail-closed: unknown and mismatch paths are pinned in the offline
    // describe of the same file (not restated here).
    expect(opener).toContain("'unknown'");
    // Pre-deploy skip is in-test on STAGING_BASE_URL only — no file-level
    // skip, so the offline verdict pins still run via test:unit.
    expect(opener).toContain('STAGING_BASE_URL not configured (pre-deploy)');
    expect(opener).not.toMatch(/^test\.skip\(\s*\n?\s*!process\.env\.STAGING_BASE_URL/m);

    const closer = readFileSync(
      new URL('../tests/zz-build-identity-final.spec.ts', import.meta.url),
      'utf8',
    );
    expect(closer).toContain(
      'deployed build still matches the promotion SHA at journey end (superseded-deploy guard)',
    );
    // A mid-journey deploy voids the evidence: the closing leg names the
    // supersession instead of reporting a bare mismatch.
    expect(closer).toContain('superseded by a newer deploy');
    expect(closer).toContain('STAGING_BASE_URL not configured (pre-deploy)');
    expect(closer).not.toMatch(/^test\.skip\(\s*\n?\s*!process\.env\.STAGING_BASE_URL/m);
  });

  test('closing identity check runs last under the serial config', () => {
    // Playwright runs test files alphabetically with a single worker and no
    // parallelism, so the `zz-` prefix sorts the closing read after every
    // other spec. Pin the config and the sort position so a future
    // parallelization or rename cannot silently move the guard mid-journey.
    const configSource = readFileSync(
      new URL('../playwright.config.ts', import.meta.url),
      'utf8',
    );
    expect(configSource).toMatch(/workers:\s*1/);
    expect(configSource).toMatch(/fullyParallel:\s*false/);
    const specs = readdirSync(fileURLToPath(new URL('../tests/', import.meta.url))).filter(
      (name) => name.endsWith('.spec.ts'),
    );
    expect(specs).toContain('zz-build-identity-final.spec.ts');
    for (const name of specs) {
      expect(
        name <= 'zz-build-identity-final.spec.ts',
        `${name} must sort before the closing identity check`,
      ).toBe(true);
    }
  });

  test('synthetic smoke stays health-only (monitoring cannot mint same-build evidence)', () => {
    const pkg = JSON.parse(
      readFileSync(new URL('../package.json', import.meta.url), 'utf8'),
    ) as { scripts: Record<string, string> };
    expect(pkg.scripts['e2e:smoke']).toContain('tests/health.spec.ts');
    expect(pkg.scripts['e2e:smoke']).not.toContain('build-identity');
    expect(pkg.scripts['test:unit']).toContain('tests/build-identity.spec.ts');
    expect(pkg.scripts['test:unit']).toContain('tests/zz-build-identity-final.spec.ts');
  });
});

// Journey evidence gate: an all-skipped Playwright run still exits 0, so
// without this gate "staging passed" could mean "nothing ran". The e2e job
// parses the junit artifact and fails closed on zero/empty/all-skipped
// evidence, on skips above (or outside) the documented allow-list, and on a
// configured run missing the QA seam token — while publishing ran / skipped /
// failed counts to the job summary. The tests below execute the exact
// checked-in script (not a copy of its logic) against synthetic junit
// fixtures. Only dummy inputs enter the child; no ambient credentials leak in
// (the child environment is fully specified, never inherited).
const gateScript = new URL('../scripts/enforce-journey-evidence.mjs', import.meta.url);
const gateScriptSource = readFileSync(gateScript, 'utf8');

function runGateScript(
  junitXml: string | null,
  env: Record<string, string | undefined> = {},
): { status: number | null; output: string; summary: string } {
  const probe = mkdtempSync(join(process.env.PAPERCLIP_RUN_SCRATCH_DIR || tmpdir(), 'journey-gate-'));
  try {
    mkdirSync(join(probe, 'test-results'), { recursive: true });
    if (junitXml !== null) writeFileSync(join(probe, 'test-results', 'junit.xml'), junitXml);
    const summaryFile = join(probe, 'summary.md');
    const result = spawnSync(process.execPath, [fileURLToPath(gateScript)], {
      encoding: 'utf8',
      timeout: 10_000,
      cwd: probe,
      env: {
        PATH: process.env.PATH,
        JUNIT_PATH: 'test-results/junit.xml',
        MAX_ALLOWED_SKIPS: '1',
        REQUIRE_QA_AUTH_TOKEN: 'false',
        GITHUB_STEP_SUMMARY: summaryFile,
        ...env,
      },
    });
    expect(result.error).toBeUndefined();
    expect(result.signal).toBeNull();
    let summary = '';
    try {
      summary = readFileSync(summaryFile, 'utf8');
    } catch {
      summary = '';
    }
    return { status: result.status, output: result.stdout + result.stderr, summary };
  } finally {
    rmSync(probe, { recursive: true, force: true });
  }
}

const FIXME_QUEUE =
  '<testcase classname="player-queue-order.spec.ts" name="player queue advances in library-selection order"><skipped message="fixme"/></testcase>';
const FIXME_MBIDS =
  '<testcase classname="ingest-play-meter.spec.ts" name="ingested track carries recording_mbids"><skipped message="fixme"/></testcase>';
const PASSING = (file: string, name: string) =>
  `<testcase classname="${file}" name="${name}"/>`;

function suiteXml(testcases: string): string {
  const tests = (testcases.match(/<testcase\b/g) ?? []).length;
  const skipped = (testcases.match(/<skipped[\s>/]/g) ?? []).length;
  const failures = (testcases.match(/<failure[\s>/]/g) ?? []).length;
  return (
    '<?xml version="1.0" encoding="UTF-8"?>\n<testsuites>\n' +
    `<testsuite name="staging" tests="${tests}" failures="${failures}" errors="0" skipped="${skipped}">\n` +
    testcases +
    '\n</testsuite>\n</testsuites>\n'
  );
}

test.describe('journey evidence gate (all-skipped must fail)', () => {
  test('journey job carries an always-run evidence gate on configured runs', () => {
    const section = jobSection(stagingWorkflow, 'e2e');
    const gateAt = section.indexOf('Enforce journey evidence gate');
    expect(gateAt, 'e2e job must carry the evidence gate step').toBeGreaterThan(-1);
    const runAt = section.indexOf('run: npm run e2e:staging');
    expect(runAt, 'e2e job must still run the full journey').toBeGreaterThan(-1);
    expect(gateAt, 'gate must run after the full journey').toBeGreaterThan(runAt);
    const nextStep = section.indexOf('\n      - name:', gateAt + 1);
    const stepBlock = section.slice(gateAt, nextStep === -1 ? undefined : nextStep);
    // Runs even when the journey failed (to publish counts) but never
    // pre-deploy (unconfigured staging produces no evidence by design).
    expect(stepBlock).toContain('if: always()');
    expect(stepBlock).toContain(`steps.staging.outputs.configured == 'true'`);
    expect(stepBlock).toContain('node scripts/enforce-journey-evidence.mjs');
    expect(stepBlock).toContain(`MAX_ALLOWED_SKIPS: '1'`);
    expect(stepBlock).toContain(`REQUIRE_QA_AUTH_TOKEN: 'true'`);
  });

  test('all-skipped evidence fails and reports empty counts', () => {
    const xml = suiteXml(
      PASSING('a.spec.ts', 'journey one').replace('/>', '><skipped message="seam unconfigured"/></testcase>') +
        PASSING('b.spec.ts', 'journey two').replace('/>', '><skipped message="seam unconfigured"/></testcase>'),
    );
    const result = runGateScript(xml);
    expect(result.status).not.toBe(0);
    expect(result.output).toContain('every journey skipped');
    expect(result.summary).toContain('| 2 | 0 | 2 | 0 |');
  });

  test('zero test cases fail as zero journeys ran', () => {
    const xml = suiteXml('');
    const result = runGateScript(xml);
    expect(result.status).not.toBe(0);
    expect(result.output).toContain('zero journeys ran');
  });

  test('only the recording_mbids fixme passes within budget with counts', () => {
    const xml = suiteXml(
      PASSING('signup-byop.spec.ts', 'signup creates an account') +
        PASSING('player-queue-order.spec.ts', 'player queue advances in library-selection order') +
        FIXME_MBIDS,
    );
    const result = runGateScript(xml);
    expect(result.status).toBe(0);
    expect(result.summary).toContain('| 3 | 2 | 1 | 0 |');
    expect(result.summary).toContain('PASS');
  });

  test('a queue-order skip is no longer allowed, even within budget', () => {
    const xml = suiteXml(PASSING('health.spec.ts', 'passing journey') + FIXME_QUEUE);
    const result = runGateScript(xml);
    expect(result.status).not.toBe(0);
    expect(result.output).toContain('outside the allow-list');
    expect(result.output).toContain('player-queue-order.spec.ts player queue advances');
  });

  test('the script default budget is one when MAX_ALLOWED_SKIPS is unset', () => {
    const xml = suiteXml(PASSING('health.spec.ts', 'passing journey') + FIXME_MBIDS + FIXME_MBIDS);
    const result = runGateScript(xml, { MAX_ALLOWED_SKIPS: undefined });
    expect(result.status).not.toBe(0);
    expect(result.output).toContain('above the allow-list budget of 1');
  });

  test('a third skip still fails above the budget', () => {
    const xml = suiteXml(
      PASSING('health.spec.ts', 'GET /api/health reports ok:true') +
        FIXME_QUEUE +
        FIXME_MBIDS +
        FIXME_QUEUE.replace('player-queue-order.spec.ts', 'other.spec.ts'),
    );
    const result = runGateScript(xml);
    expect(result.status).not.toBe(0);
    expect(result.output).toContain('above the allow-list budget');
  });

  test('a skip outside the allow-list fails and names the test', () => {
    const xml = suiteXml(
      PASSING('health.spec.ts', 'GET /api/health reports ok:true') +
        '<testcase classname="scoped-credentials.spec.ts" name="lifecycle mints a session"><skipped message="no token"/></testcase>',
    );
    const result = runGateScript(xml);
    expect(result.status).not.toBe(0);
    expect(result.output).toContain('outside the allow-list');
    expect(result.output).toContain('scoped-credentials.spec.ts lifecycle mints a session');
  });

  test('failures fail the gate without leaking failure text', () => {
    const marker = 'SENTINEL-FAILURE-VALUE-GATE';
    const xml = suiteXml(
      PASSING('health.spec.ts', 'GET /api/health reports ok:true') +
        `<testcase classname="ingest.spec.ts" name="NZB ingests"><failure message="boom">${marker} in failure text</failure></testcase>`,
    );
    const result = runGateScript(xml);
    expect(result.status).not.toBe(0);
    expect(result.output).toContain('failing test(s)');
    expect(result.output).not.toContain(marker);
    expect(result.summary).not.toContain(marker);
  });

  test('absent junit evidence on a configured run fails closed', () => {
    const result = runGateScript(null, { JUNIT_PATH: 'test-results/absent.xml' });
    expect(result.status).not.toBe(0);
    expect(result.output).toContain('no junit evidence');
  });

  test('missing seam token fails a token-required run, passes otherwise', () => {
    const xml = suiteXml(PASSING('health.spec.ts', 'GET /api/health reports ok:true'));
    const required = runGateScript(xml, { REQUIRE_QA_AUTH_TOKEN: 'true', QA_AUTH_TOKEN: '' });
    expect(required.status).not.toBe(0);
    expect(required.output).toContain('QA_AUTH_TOKEN is not set');
    // A token-missing red run still publishes the counts receipt.
    expect(required.summary).toContain('| 1 | 1 | 0 | 0 |');
    expect(required.summary).toContain('seam token: missing');
    const provided = runGateScript(xml, {
      REQUIRE_QA_AUTH_TOKEN: 'true',
      QA_AUTH_TOKEN: 'dummy-qa-token',
    });
    expect(provided.status).toBe(0);
    const notRequired = runGateScript(xml, { REQUIRE_QA_AUTH_TOKEN: 'false', QA_AUTH_TOKEN: '' });
    expect(notRequired.status).toBe(0);
  });

  test('allow-list reasons name the owning specs', () => {
    expect(gateScriptSource).not.toContain('player-queue-order');
    expect(gateScriptSource).toContain('ingest-play-meter');
    expect(gateScriptSource).toContain('reason');
  });

  // Reporter-shaped fixtures below mirror the exact @playwright/test 1.64
  // junit reporter: a <testsuites> root carrying tests/failures/skipped/errors
  // counts, one <testsuite> per spec file, name-before-classname attributes,
  // describe-prefixed test names ("suite › test"), <properties> fixme markers
  // and paired <skipped></skipped> elements. Synthetic suiteXml() above uses
  // none of that shape, so without these a parser regression against the real
  // reporter would stay green here while misreading staging evidence.
  const REPORTER_GREEN =
    '<?xml version="1.0" encoding="UTF-8"?>\n' +
    '<testsuites id="" name="" tests="4" failures="0" skipped="1" errors="0" time="0.27">\n' +
    '<testsuite name="ingest-play-meter.spec.ts" timestamp="2026-10-19T03:09:03.424Z" hostname="" tests="2" failures="0" skipped="1" time="0.002" errors="0">\n' +
    '<testcase name="ingest play meter browser › passing journey" classname="ingest-play-meter.spec.ts" time="0.002">\n' +
    '<properties>\n<property name="serial" value="">\n</property>\n</properties>\n' +
    '</testcase>\n' +
    '<testcase name="ingest play meter browser › ingested track carries recording_mbids" classname="ingest-play-meter.spec.ts">\n' +
    '<properties>\n<property name="serial" value="">\n</property>\n<property name="fixme" value="">\n</property>\n</properties>\n' +
    '<skipped>\n</skipped>\n' +
    '</testcase>\n' +
    '</testsuite>\n' +
    '<testsuite name="player-queue-order.spec.ts" timestamp="2026-10-19T03:09:03.424Z" hostname="" tests="2" failures="0" skipped="0" time="0" errors="0">\n' +
    '<testcase name="queue order › passing journey" classname="player-queue-order.spec.ts" time="0.001">\n' +
    '<properties>\n<property name="serial" value="">\n</property>\n</properties>\n' +
    '</testcase>\n' +
    '<testcase name="player queue follows library-selection order › player queue advances in library-selection order" classname="player-queue-order.spec.ts" time="0.001">\n' +
    '<properties>\n<property name="serial" value="">\n</property>\n</properties>\n' +
    '</testcase>\n' +
    '</testsuite>\n' +
    '</testsuites>\n';

  const REPORTER_THROWN_ERROR_MARKER = 'SENTINEL-THROWN-VALUE-GATE';
  const REPORTER_THROWN_ERROR =
    '<?xml version="1.0" encoding="UTF-8"?>\n' +
    '<testsuites id="" name="" tests="3" failures="0" skipped="1" errors="1" time="0.28">\n' +
    '<testsuite name="ingest-play-meter.spec.ts" timestamp="2026-10-19T03:09:03.424Z" hostname="" tests="3" failures="0" skipped="1" time="0.002" errors="1">\n' +
    '<testcase name="ingest play meter browser › passing journey" classname="ingest-play-meter.spec.ts" time="0.002">\n' +
    '</testcase>\n' +
    '<testcase name="ingest play meter browser › ingested track carries recording_mbids" classname="ingest-play-meter.spec.ts">\n' +
    '<properties>\n<property name="fixme" value="">\n</property>\n</properties>\n' +
    '<skipped>\n</skipped>\n' +
    '</testcase>\n' +
    `<testcase name="thrown error journey" classname="ingest-play-meter.spec.ts" time="0">\n` +
    '<system-out>\n<![CDATA[\n[[ATTACHMENT|../../test-results/sample-thrown-error-journey/error-context.md]]\n]]>\n</system-out>\n' +
    `<error message="boom" type="Error">\n<![CDATA[thrown error journey ${REPORTER_THROWN_ERROR_MARKER} in error text]]>\n</error>\n` +
    '</testcase>\n' +
    '</testsuite>\n' +
    '</testsuites>\n';

  test('reporter-shaped green evidence with root counts and paired skips passes', () => {
    const result = runGateScript(REPORTER_GREEN);
    expect(result.status).toBe(0);
    expect(result.summary).toContain('| 4 | 3 | 1 | 0 |');
    expect(result.summary).toContain('PASS');
  });

  test('a thrown Error (errors=1, failures=0) fails without leaking error text', () => {
    const result = runGateScript(REPORTER_THROWN_ERROR);
    expect(result.status).not.toBe(0);
    expect(result.output).toContain('failing test(s)');
    expect(result.output).not.toContain(REPORTER_THROWN_ERROR_MARKER);
    expect(result.summary).not.toContain(REPORTER_THROWN_ERROR_MARKER);
    expect(result.summary).toContain('| 3 | 2 | 1 | 1 |');
  });

  test('a same-titled skip from another file fails outside the allow-list', () => {
    const xml = suiteXml(
      PASSING('health.spec.ts', 'GET /api/health reports ok:true') +
        FIXME_MBIDS.replace('ingest-play-meter.spec.ts', 'other.spec.ts'),
    );
    const result = runGateScript(xml);
    expect(result.status).not.toBe(0);
    expect(result.output).toContain('outside the allow-list');
  });

  test('one allow-list entry cannot cover two skips', () => {
    const xml = suiteXml(
      PASSING('health.spec.ts', 'GET /api/health reports ok:true') +
        FIXME_MBIDS +
        FIXME_MBIDS,
    );
    // Raise only the count ceiling to isolate the consume-once allowance.
    const result = runGateScript(xml, { MAX_ALLOWED_SKIPS: '2' });
    expect(result.status).not.toBe(0);
    expect(result.output).toContain('outside the allow-list');
  });

  test('unattributed skips fail closed when the count exceeds listed cases', () => {
    const listed = suiteXml(
      PASSING('health.spec.ts', 'GET /api/health reports ok:true') +
        FIXME_MBIDS,
    );
    const xml = listed.replace('tests="2"', 'tests="3"').replace('skipped="1"', 'skipped="2"');
    const result = runGateScript(xml, { MAX_ALLOWED_SKIPS: '5' });
    expect(result.status).not.toBe(0);
    expect(result.output).toContain('could not be identified');
  });

  test('non-numeric counts fail closed as unparseable', () => {
    const xml = suiteXml(FIXME_MBIDS).replace('skipped="1"', 'skipped="x"');
    const result = runGateScript(xml);
    expect(result.status).not.toBe(0);
    expect(result.output).toContain('could not parse');
  });

});

// mb-import postgres parity: the integration job's service image must stay
// the same postgres build the shipped image is based on. ci.yml carries a
// tying comment (re-pin both deliberately) but no enforcement, so a
// one-sided re-pin would silently test against a newer Postgres than the
// image ships. This pin asserts exact digest equality (after the `@`)
// between the mb-import job's service image and the Dockerfile's runtime
// base FROM, plus tag equality so a tag-only drift cannot hide behind a
// matching digest. Deliberate bumps change both sides together and stay
// green; changing either side alone reds.
const mbImportDockerfile = readFileSync(
  new URL('../../containers/mb-import/Dockerfile', import.meta.url),
  'utf8',
);

function mbImportBaseDigest(dockerfile: string): { tag: string; digest: string } {
  // Anchored `FROM … AS base`: the runtime base, never the rust validator
  // stage and never a commented-out line (`# FROM` cannot match `^\s*FROM`).
  const matches = [
    ...dockerfile.matchAll(
      /^\s*FROM\s+postgres:([^\s@]+)@(sha256:[0-9a-f]{64})\s+AS\s+base\b/gim,
    ),
  ];
  expect(matches, 'mb-import Dockerfile must carry exactly one postgres AS base FROM').toHaveLength(1);
  return { tag: matches[0]![1]!, digest: matches[0]![2]! };
}

function mbImportServiceDigest(mbImportSection: string): { tag: string; digest: string } {
  // Anchored `image:` so a commented-out service line cannot satisfy the pin.
  const matches = [
    ...mbImportSection.matchAll(
      /^\s*image:\s*postgres:([^\s@]+)@(sha256:[0-9a-f]{64})\b/gim,
    ),
  ];
  expect(matches, 'mb-import job must carry exactly one postgres service image').toHaveLength(1);
  return { tag: matches[0]![1]!, digest: matches[0]![2]! };
}

function assertMbImportPostgresParity(ciWorkflowText: string, dockerfileText: string): void {
  const service = mbImportServiceDigest(jobSection(ciWorkflowText, 'mb-import'));
  const base = mbImportBaseDigest(dockerfileText);
  expect(
    service.tag,
    `mb-import postgres tag drift: CI services ${service.tag} vs Dockerfile FROM ${base.tag} (re-pin both deliberately)`,
  ).toBe(base.tag);
  expect(
    service.digest,
    `mb-import postgres digest drift: CI services ${service.digest} vs Dockerfile FROM ${base.digest} (re-pin both deliberately)`,
  ).toBe(base.digest);
}

test.describe('mb-import postgres parity (static pin)', () => {
  test('CI service image digest equals the shipped Dockerfile base digest', () => {
    assertMbImportPostgresParity(ciWorkflow, mbImportDockerfile);
  });

  test('single-side digest or tag mutants fail the pin', () => {
    // Regression proof for the shared guard: each mutant changes one side
    // alone (valid hex/tag so extraction still succeeds) and must throw
    // the same assert the real pin calls, not a restated copy.
    const base = mbImportBaseDigest(mbImportDockerfile);
    const flippedLast = base.digest.endsWith('0') ? '1' : '0';
    const mutantDigest = base.digest.slice(0, -1) + flippedLast;
    // Derived from the live tag so a deliberate re-tag never turns the tag
    // mutants into no-ops that red on a correct change.
    const mutantTag = `${base.tag}-mutant`;
    const ciImageLine = `image: postgres:${base.tag}@${base.digest}`;
    const dockerFromLine = `FROM postgres:${base.tag}@${base.digest} AS base`;
    expect(ciWorkflow, 'CI fixture line for the mutant is missing').toContain(ciImageLine);
    expect(mbImportDockerfile, 'Dockerfile fixture line for the mutant is missing').toContain(
      dockerFromLine,
    );
    const ciDigestMutant = ciWorkflow.replace(
      ciImageLine,
      `image: postgres:${base.tag}@${mutantDigest}`,
    );
    const dockerDigestMutant = mbImportDockerfile.replace(
      dockerFromLine,
      `FROM postgres:${base.tag}@${mutantDigest} AS base`,
    );
    const ciTagMutant = ciWorkflow.replace(
      ciImageLine,
      `image: postgres:${mutantTag}@${base.digest}`,
    );
    const dockerTagMutant = mbImportDockerfile.replace(
      dockerFromLine,
      `FROM postgres:${mutantTag}@${base.digest} AS base`,
    );
    for (const [name, ci, docker] of [
      ['CI digest', ciDigestMutant, mbImportDockerfile],
      ['Dockerfile digest', ciWorkflow, dockerDigestMutant],
      ['CI tag', ciTagMutant, mbImportDockerfile],
      ['Dockerfile tag', ciWorkflow, dockerTagMutant],
    ] as const) {
      expect(() => assertMbImportPostgresParity(ci, docker), `${name} mutant must fail the pin`).toThrow();
    }
  });
});

// Bounded CI downloads (static pin): the cargo-deny and gitleaks installs
// are checksum-pinned but their curl lines carry the only network bounds in
// those steps. No other test reads these lines, so a revert to the
// unbounded forms would stay green without these pins. Each pin slices its
// own step section, so an identical curl added elsewhere cannot satisfy it.
const gitleaksWorkflow = readFileSync(new URL('gitleaks.yml', workflowDir), 'utf8');

function stepSection(workflow: string, stepName: string): string {
  const marker = `- name: ${stepName}\n`;
  const start = workflow.indexOf(marker);
  expect(start, `step ${stepName} not found`).toBeGreaterThan(-1);
  const bodyStart = start + marker.length;
  const next = /\n      - /.exec(workflow.slice(bodyStart));
  return workflow.slice(bodyStart, next ? bodyStart + next.index : undefined);
}

function assertCargoDenyDownloadBounded(section: string): void {
  expect(section).toContain('curl -fsSL --connect-timeout 10 --max-time 30 --retry 3 \\');
  expect(section).toContain('| (cd /tmp && sha256sum -c -)');
}

function assertGitleaksDownloadBounded(section: string): void {
  expect(section).toContain(
    'curl -sSfL --connect-timeout 10 --max-time 30 --retry 3 -o "$archive" \\',
  );
  expect(section).toContain('| sha256sum --check -');
}

test.describe('bounded CI downloads (static pin)', () => {
  test('cargo-deny download fails HTTP errors and stays within 10/30/3', () => {
    assertCargoDenyDownloadBounded(
      stepSection(ciWorkflow, 'Install pinned cargo-deny 0.20.2'),
    );
  });

  test('gitleaks download stays within 10/30/3', () => {
    assertGitleaksDownloadBounded(stepSection(gitleaksWorkflow, 'Install gitleaks'));
  });

  test('cargo-deny flag-removal and value mutants fail the pin', () => {
    const section = stepSection(ciWorkflow, 'Install pinned cargo-deny 0.20.2');
    const mutants = [
      // Exact base revert: unbounded single-line download without -f.
      section.replace(
        'curl -fsSL --connect-timeout 10 --max-time 30 --retry 3 \\\n            -o "/tmp/${asset}" "${base}/${asset}"',
        'curl -sSL --retry 3 -o "/tmp/${asset}" "${base}/${asset}"',
      ),
      section.replace('curl -fsSL ', 'curl -sSL '),
      section.replace(' --connect-timeout 10', ''),
      section.replace(' --max-time 30', ''),
      section.replace(' --retry 3', ''),
      section.replace('--connect-timeout 10', '--connect-timeout 5'),
      section.replace('--max-time 30', '--max-time 60'),
      section.replace('--retry 3', '--retry 5'),
      section.replace('| (cd /tmp && sha256sum -c -)', '| cat'),
    ];
    for (const mutant of mutants) {
      expect(mutant, 'mutant must differ from the pinned section').not.toBe(section);
      expect(() => assertCargoDenyDownloadBounded(mutant), 'mutant must violate the pin').toThrow();
    }
  });

  test('gitleaks flag-removal and value mutants fail the pin', () => {
    const section = stepSection(gitleaksWorkflow, 'Install gitleaks');
    const mutants = [
      // Exact base revert: unbounded download without timeouts or retry.
      section.replace(
        'curl -sSfL --connect-timeout 10 --max-time 30 --retry 3 -o "$archive" \\',
        'curl -sSfL -o "$archive" \\',
      ),
      section.replace(' --connect-timeout 10', ''),
      section.replace(' --max-time 30', ''),
      section.replace(' --retry 3', ''),
      section.replace('--connect-timeout 10', '--connect-timeout 5'),
      section.replace('--max-time 30', '--max-time 60'),
      section.replace('--retry 3', '--retry 5'),
      section.replace('| sha256sum --check -', '| cat'),
    ];
    for (const mutant of mutants) {
      expect(mutant, 'mutant must differ from the pinned section').not.toBe(section);
      expect(() => assertGitleaksDownloadBounded(mutant), 'mutant must violate the pin').toThrow();
    }
  });
});

// Burst probe bounds (static + executed pin): the diagnostic probe must run
// under `set -euo pipefail`, bound each request to 10/30 seconds, loop
// exactly 11 attempts, and never retry (one physical request per attempt).
// Static pins catch flag edits; the executed fixture runs the real script
// against a stubbed `curl` on PATH — no network, INGEST_URL is never
// dialed — and proves 11 single-attempt requests, exit 0 on a final
// 429/503, and fail-fast abort on transport failure.
const burstScriptPath = fileURLToPath(
  new URL('../../workers/ingest/scripts/search_rate_limit_burst.sh', import.meta.url),
);
const burstScript = readFileSync(burstScriptPath, 'utf8');

function assertBurstProbeStatic(source: string): void {
  expect(source).toContain('set -euo pipefail');
  expect(source).toContain(
    "curl -s --connect-timeout 10 -m 30 -o /dev/null -D - -w '%{http_code}\\n' \\",
  );
  expect(source).toContain('for i in $(seq 1 11);');
  expect(source).not.toContain('--retry');
}

interface BurstRun {
  status: number | null;
  output: string;
  attempts: number;
  invocations: string[];
}

function runBurstProbe(scriptPath: string, stubEnv: Record<string, string>): BurstRun {
  const probe = mkdtempSync(
    join(process.env.PAPERCLIP_RUN_SCRATCH_DIR || tmpdir(), 'burst-pin-'),
  );
  try {
    // Stub curl: records each invocation's args, serves 202 until attempt
    // 11, then the configured final code; STUB_FAIL_AT makes attempt N a
    // transport failure (exit 7, like a refused connection).
    writeFileSync(
      join(probe, 'curl'),
      `#!/bin/bash
n=$(cat "$STUB_COUNT" 2>/dev/null || echo 0)
n=$((n + 1))
echo "$n" > "$STUB_COUNT"
printf '%s\\n' "$*" >> "$STUB_RECORD"
if [ "$STUB_FAIL_AT" = "$n" ]; then exit 7; fi
if [ "$n" -ge 11 ]; then code="$STUB_FINAL_CODE"; else code=202; fi
if [ "$code" = "429" ]; then printf 'retry-after: 2\\n'; fi
printf '%s\\n' "$code"
`,
    );
    chmodSync(join(probe, 'curl'), 0o755);
    const result = spawnSync('/bin/bash', [scriptPath], {
      encoding: 'utf8',
      timeout: 10_000,
      cwd: probe,
      env: {
        PATH: `${probe}:${process.env.PATH}`,
        INGEST_URL: 'http://stub.invalid',
        STUB_RECORD: join(probe, 'curl-args.log'),
        STUB_COUNT: join(probe, 'curl-count'),
        STUB_FINAL_CODE: '429',
        STUB_FAIL_AT: '0',
        ...stubEnv,
      },
    });
    expect(result.error).toBeUndefined();
    expect(result.signal).toBeNull();
    const output = result.stdout + result.stderr;
    let invocations: string[] = [];
    try {
      invocations = readFileSync(join(probe, 'curl-args.log'), 'utf8')
        .split('\n')
        .filter((line) => line.length > 0);
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
    }
    const attempts = output.match(/^attempt \d\d: /gm)?.length ?? 0;
    return { status: result.status, output, attempts, invocations };
  } finally {
    rmSync(probe, { recursive: true, force: true });
  }
}

function assertBurstExecutedContract(run: BurstRun, finalCode: string): void {
  expect(run.status).toBe(0);
  expect(run.attempts).toBe(11);
  // One physical request per attempt: exactly 11 stub invocations, each
  // carrying the 10/30-second bounds and no retry flag.
  expect(run.invocations).toHaveLength(11);
  for (const args of run.invocations) {
    expect(args).toContain('--connect-timeout 10');
    expect(args).toContain('-m 30');
    expect(args).not.toContain('--retry');
  }
  expect(run.output).toContain(finalCode);
}

test.describe('burst probe bounds (static + executed pin)', () => {
  test('probe runs strict with bounded single-attempt requests', () => {
    assertBurstProbeStatic(burstScript);
  });

  test('final 429 completes 11 requests and exits 0', () => {
    const run = runBurstProbe(burstScriptPath, { STUB_FINAL_CODE: '429' });
    assertBurstExecutedContract(run, '429');
    expect(run.output).toContain('retry-after');
  });

  test('final 503 stays observable and exits 0', () => {
    const run = runBurstProbe(burstScriptPath, { STUB_FINAL_CODE: '503' });
    assertBurstExecutedContract(run, '503');
  });

  test('transport failure aborts on the first attempt', () => {
    const run = runBurstProbe(burstScriptPath, { STUB_FAIL_AT: '1' });
    expect(run.status).not.toBe(0);
    expect(run.attempts).toBe(1);
    expect(run.invocations).toHaveLength(1);
  });

  test('flag-removal, value, and retry mutants fail the static pin', () => {
    const mutants = [
      burstScript.replace('set -euo pipefail', 'set -u'),
      burstScript.replace(' --connect-timeout 10', ''),
      burstScript.replace(' -m 30', ''),
      burstScript.replace(' -m 30', ' -m 30 --retry 3'),
      burstScript.replace('--connect-timeout 10', '--connect-timeout 5'),
      burstScript.replace('-m 30', '-m 60'),
      burstScript.replace('seq 1 11', 'seq 1 5'),
    ];
    for (const mutant of mutants) {
      expect(mutant, 'mutant must differ from the pinned script').not.toBe(burstScript);
      expect(() => assertBurstProbeStatic(mutant), 'mutant must violate the pin').toThrow();
    }
  });

  test('base probe fails open, proving the executed fixture distinguishes it', () => {
    // Regression proof: the pre-change probe (set -u, unbounded curl) run
    // through the same stub exits 0 on a first-attempt transport failure
    // and records unbounded invocations — the executed contract throws on it.
    const base = burstScript
      .replace('set -euo pipefail', 'set -u')
      .replace(' --connect-timeout 10 -m 30', '');
    expect(base).not.toBe(burstScript);
    expect(() => assertBurstProbeStatic(base), 'base text must violate the static pin').toThrow();
    const probe = mkdtempSync(
      join(process.env.PAPERCLIP_RUN_SCRATCH_DIR || tmpdir(), 'burst-base-'),
    );
    try {
      const mutantPath = join(probe, 'burst-base.sh');
      writeFileSync(mutantPath, base);
      const failOpen = runBurstProbe(mutantPath, { STUB_FAIL_AT: '1' });
      expect(failOpen.status).toBe(0);
      expect(failOpen.invocations).toHaveLength(11);
      expect(failOpen.invocations[0]).not.toContain('--connect-timeout');
      expect(
        () => assertBurstExecutedContract(failOpen, '429'),
        'base run must violate the executed contract',
      ).toThrow();
    } finally {
      rmSync(probe, { recursive: true, force: true });
    }
  });

  test('ingest README documents the local-only probe', () => {
    const readme = readFileSync(
      new URL('../../workers/ingest/README.md', import.meta.url),
      'utf8',
    );
    expect(readme).toContain('scripts/search_rate_limit_burst.sh');
    expect(readme).toContain('never point this script at');
    expect(readme).toContain('production');
  });
});
