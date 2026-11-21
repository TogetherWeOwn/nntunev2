import { expect, request as playwrightRequest, test, type APIResponse } from '@playwright/test';
import { accessHeaders, readApiBody, stagingBaseUrl } from '../helpers/access.js';
import { apiDelete, apiPost, SESSION_COOKIE_NAME, tryMintTestAccount } from '../helpers/accounts.js';
import { assertServingBuildMatchesPromotion } from '../helpers/build-identity-live.js';
import { apiDeployOutcome } from '../helpers/build-identity.js';

const CONTEXT = 'listen-history-privacy';
const EXPORT_PATH = '/v1/plays/export';
const CLEAR_PATH = '/v1/plays/events';

type Headers = Record<string, string>;

async function historyJson(
  response: APIResponse,
  context: string,
  status: number,
): Promise<Record<string, unknown>> {
  const text = await readApiBody(response, '', context);
  expect(response.status(), `${context}: Worker status`).toBe(status);
  const cacheControl = response.headers()['cache-control'] ?? '';
  expect(
    cacheControl.split(',').some((directive) => directive.trim().toLowerCase() === 'no-store'),
    `${context}: Cache-Control must include no-store`,
  ).toBe(true);
  try {
    const body: unknown = JSON.parse(text);
    if (body && typeof body === 'object' && !Array.isArray(body)) {
      return body as Record<string, unknown>;
    }
  } catch {
    // Do not propagate parser errors; they may quote account or row data.
  }
  throw new Error(`[${context}] expected a JSON object (body not logged).`);
}

async function expectEmptyExport(response: APIResponse, context: string): Promise<void> {
  const { events, next_cursor, retention } = await historyJson(response, context, 200);
  // Assert predicates only: a failure must not print a row, cursor or raw value.
  expect(retention === 'account_lifetime', `${context}: account-lifetime retention`).toBe(true);
  expect(Array.isArray(events) && events.length === 0, `${context}: empty fixture events`).toBe(true);
  expect(next_cursor === null, `${context}: no next cursor for an empty page`).toBe(true);
}

async function expectAuthRejection(response: APIResponse, context: string): Promise<void> {
  const body = await historyJson(response, context, 401);
  expect(
    body.error === 'check your email and try again',
    `${context}: Worker auth rejection`,
  ).toBe(true);
}

test('empty-history privacy-route smoke: export, reject, clear, export again', async ({ request }) => {
  const baseURL = stagingBaseUrl('skip', CONTEXT);
  if (!baseURL) {
    test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
    return;
  }
  const target = URL.canParse(baseURL) ? new URL(baseURL) : null;
  if (
    !target || target.protocol !== 'https:' || target.username || target.password ||
    target.origin !== baseURL
  ) {
    throw new Error(`[${CONTEXT}] STAGING_BASE_URL must be a bare HTTPS origin.`);
  }
  const origin = target.origin;
  if (apiDeployOutcome() !== 'deployed') {
    throw new Error(`[${CONTEXT}] requires the ordinary post-deploy journey with staging-api deployed.`);
  }
  await assertServingBuildMatchesPromotion(request, origin, CONTEXT);

  // The seam mints a new account and refuses existing mailboxes. Its setup
  // bearer stays inside the mint call; the privacy routes get only the cookie.
  const account = await tryMintTestAccount(request, origin);
  if (!account?.session) {
    throw new Error(`[${CONTEXT}] QA_AUTH_TOKEN and a freshly minted account session are required.`);
  }
  const edgeHeaders = accessHeaders();
  const sessionHeaders = { ...edgeHeaders, Cookie: `${SESSION_COOKIE_NAME}=${account.session}` };
  const exactHeaders = { ...sessionHeaders, Origin: origin };
  const exportHistory = (client: typeof request, headers: Headers, context: string) =>
    apiPost(client, `${origin}${EXPORT_PATH}`, headers, {}, context);
  const clearHistory = (client: typeof request, headers: Headers, context: string) =>
    apiDelete(client, `${origin}${CLEAR_PATH}`, headers, context);

  // Stop before any DELETE if the fixture is unexpectedly populated. This
  // smoke does not seed, remove or prove isolation of populated history.
  const before = await exportHistory(request, exactHeaders, 'initial export');
  await expectEmptyExport(before, 'initial export');

  for (const [label, send] of [['export', exportHistory], ['clear', clearHistory]] as const) {
    const noCookie = `${label} without cookie`;
    // A fresh jar for each probe cannot inherit a cookie from mint or export.
    const anonymous = await playwrightRequest.newContext();
    try {
      const denied = await send(anonymous, { ...edgeHeaders, Origin: origin }, noCookie);
      await expectAuthRejection(denied, noCookie);
    } finally {
      await anonymous.dispose();
    }
    // Only the Origin header changes (scheme mismatch); do not send headers
    // or credentials to another URL. The real cookie isolates the Origin gate.
    const wrongOrigin = `${label} with wrong Origin`;
    const denied = await send(request, { ...sessionHeaders, Origin: `http://${target.host}` }, wrongOrigin);
    await expectAuthRejection(denied, wrongOrigin);
  }

  const clear = await historyJson(await clearHistory(request, exactHeaders, 'clear'), 'clear', 200);
  expect(clear.ok === true, 'clear must confirm with ok:true').toBe(true);
  const after = await exportHistory(request, exactHeaders, 'post-clear export');
  await expectEmptyExport(after, 'post-clear export');
});
