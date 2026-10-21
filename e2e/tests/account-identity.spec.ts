import { test, expect, type APIRequestContext, type Page } from '@playwright/test';
import {
  accountHeaders,
  applyAccountToPage,
  requireApiAccount,
  tryMintTestAccount,
  type TestAccount,
} from '../helpers/accounts.js';

// Acceptance (offline, no staging, no real credentials): account-specific
// assertions must run as the minted account, never as the generic QA seam
// identity. These fixtures drive the SAME helpers the live specs use
// (tryMintTestAccount / requireApiAccount / accountHeaders /
// applyAccountToPage) with fake seam responses and captured outgoing
// requests — not a copy of them. Live seam acceptance is unverified and
// out of scope here.
//
// Contract the builder implements in e2e/helpers/accounts.ts (mint
// validation + account-specific guard/header behavior ONLY; redirect
// formatting and BYOP controls belong to other slices):
//
// tryMintTestAccount (seam configured, i.e. QA_AUTH_TOKEN set):
//   - A 2xx body that is JSON null, an array, or a primitive, or an object
//     whose `email` is missing, non-string, or blank/whitespace, THROWS an
//     Error whose message contains `[seam]` and names the identity problem
//     (`identity` or `email`). The email is NEVER fabricated client-side,
//     and no credential value (session/token/QA) appears in the message.
//   - `session` / `token`: the first nonblank string variant (session
//     preferred, as today) becomes the account session. Missing, blank,
//     whitespace-only, or non-string variants yield `session: undefined`
//     (a setup-only result, as documented) — never a blank-string session.
//   - Unconfigured seam (no QA_AUTH_TOKEN) still resolves null so callers
//     keep the UI-signup fallback, and no request is made.
//
// requireApiAccount(account, context):
//   - Passes ONLY when `account.session` is a nonblank string. A null
//     account, a sessionless account, and a blank/whitespace session all
//     throw (message contains `[${context}]`, no values) — even when
//     QA_AUTH_TOKEN is set. The generic QA token is seam-setup-only and
//     can never satisfy this account-identity guard.
//
// accountHeaders(account):
//   - With a nonblank session: Authorization is `Bearer <session>` and the
//     QA seam token appears nowhere in the headers.
//   - With a null, sessionless, or blank-session account: THROWS naming
//     the missing session/credential (no values) instead of falling back
//     to the QA token. Every accountHeaders caller is account-specific.
//
// applyAccountToPage(page, account, baseURL):
//   - `null` stays allowed (Access-only routing for the UI-signup fallback
//     and health checks): staging-origin requests carry the Access pair
//     only, no Authorization, no QA token.
//   - A non-null account without a nonblank session THROWS naming the
//     missing session/credential BEFORE any route is registered — a
//     sessionless minted account must not silently become the page's
//     (non-)identity for account-specific browser assertions.
//   - With a session: staging-origin requests are fetched with
//     maxRedirects: 0 carrying `Bearer <session>` (never the QA token);
//     foreign-origin requests continue unmodified (origin scoping and
//     no-follow protections preserved).
//
// Account isolation: two minted accounts (including a distinct capped /
// `plan` account) keep distinct emails and credentials; each account's
// headers carry only its own session. The seam SETUP request itself still
// carries the QA token and the requested plan — that is its one allowed
// use. No new seam, auth scheme, or endpoint is required by this file.
//
// This file intentionally has NO STAGING_BASE_URL skip guard and never
// prints sentinel values in expectations about error messages: sentinel
// strings below exist only to prove they do NOT leak.

const BASE = 'https://staging.example';
const QA_SENTINEL = 'sentinel-qa-token-do-not-leak';
const SEAM_ENV = {
  STAGING_BASE_URL: BASE,
  QA_AUTH_TOKEN: QA_SENTINEL,
  CF_ACCESS_CLIENT_ID: 'dummy-client-id',
  CF_ACCESS_CLIENT_SECRET: 'dummy-client-secret',
};

function withEnv(vars: Record<string, string | undefined>, fn: () => void): void {
  const saved: Record<string, string | undefined> = {};
  for (const key of Object.keys(vars)) {
    saved[key] = process.env[key];
    if (vars[key] === undefined) delete process.env[key];
    else process.env[key] = vars[key];
  }
  try {
    fn();
  } finally {
    for (const key of Object.keys(vars)) {
      if (saved[key] === undefined) delete process.env[key];
      else process.env[key] = saved[key];
    }
  }
}

async function withEnvAsync(
  vars: Record<string, string | undefined>,
  fn: () => Promise<void>,
): Promise<void> {
  const saved: Record<string, string | undefined> = {};
  for (const key of Object.keys(vars)) {
    saved[key] = process.env[key];
    if (vars[key] === undefined) delete process.env[key];
    else process.env[key] = vars[key];
  }
  try {
    await fn();
  } finally {
    for (const key of Object.keys(vars)) {
      if (saved[key] === undefined) delete process.env[key];
      else process.env[key] = saved[key];
    }
  }
}

interface CapturedPost {
  url: string;
  headers: Record<string, string>;
  data: Record<string, unknown>;
  maxRedirects?: number;
}

const fakeResponse = (body: string, status = 200) => ({
  status: () => status,
  headers: () => ({ 'content-type': 'application/json' }),
  text: async () => body,
});

/** Fake seam: records outgoing POSTs, answers with the queued bodies. */
function fakeSeam(bodies: string[]): { request: APIRequestContext; posts: CapturedPost[] } {
  const posts: CapturedPost[] = [];
  const queue = [...bodies];
  const post = async (url: string, opts: CapturedPost & { timeout?: number }) => {
    posts.push({ url, headers: opts.headers, data: opts.data, maxRedirects: opts.maxRedirects });
    const body = queue.shift();
    if (body === undefined) throw new Error('fake seam received more POSTs than queued bodies');
    return fakeResponse(body);
  };
  return { request: { post } as unknown as APIRequestContext, posts };
}

const sessionAccount = (overrides?: Partial<TestAccount>): TestAccount => ({
  email: 'minted@example.test',
  session: 'sentinel-session-a',
  viaApi: true,
  ...overrides,
});

test.describe('tryMintTestAccount mint-schema validation', () => {
  const malformedBodies: Array<{ name: string; body: string; sentinels?: string[] }> = [
    { name: 'JSON null', body: 'null' },
    { name: 'JSON array', body: '[]' },
    { name: 'JSON array with an object inside', body: '[{"email":"a@b","session":"s"}]' },
    { name: 'JSON string primitive', body: '"minted"' },
    { name: 'JSON number primitive', body: '42' },
    { name: 'JSON boolean primitive', body: 'true' },
    { name: 'empty object {}', body: '{}' },
    { name: 'session but no identity', body: '{"session":"sentinel-session-x"}', sentinels: ['sentinel-session-x'] },
    { name: 'token but no identity', body: '{"token":"sentinel-token-x"}', sentinels: ['sentinel-token-x'] },
    { name: 'blank email', body: '{"email":"","session":"s"}' },
    { name: 'whitespace-only email', body: '{"email":"   ","session":"s"}' },
    { name: 'non-string email', body: '{"email":123,"session":"s"}' },
    { name: 'null email with a session', body: '{"email":null,"session":"sentinel-session-y"}', sentinels: ['sentinel-session-y'] },
  ];

  for (const { name, body, sentinels } of malformedBodies) {
    test(`configured seam returning ${name} fails explicitly (no fabricated email)`, async () => {
      await withEnvAsync(SEAM_ENV, async () => {
        const { request } = fakeSeam([body]);
        let message = '';
        try {
          await tryMintTestAccount(request, BASE);
        } catch (error) {
          message = (error as Error).message;
        }
        expect(message, `mint accepted ${name} instead of failing`).not.toBe('');
        expect(message).toMatch(/\[seam\]/);
        expect(message).toMatch(/identity|email/i);
        expect(message).not.toContain(QA_SENTINEL);
        for (const sentinel of sentinels ?? []) {
          expect(message).not.toContain(sentinel);
        }
      });
    });
  }

  test('accepts the documented nonblank session and token variants consistently', async () => {
    await withEnvAsync(SEAM_ENV, async () => {
      const cases: Array<{ body: string; session: string | undefined }> = [
        { body: '{"email":"a@b","session":"sentinel-session-a"}', session: 'sentinel-session-a' },
        { body: '{"email":"a@b","token":"sentinel-token-a"}', session: 'sentinel-token-a' },
        // session keeps precedence when both are nonblank (existing contract)
        { body: '{"email":"a@b","session":"sentinel-session-a","token":"sentinel-token-a"}', session: 'sentinel-session-a' },
        // a blank session must not shadow a usable token variant
        { body: '{"email":"a@b","session":"","token":"sentinel-token-a"}', session: 'sentinel-token-a' },
        { body: '{"email":"a@b","session":"   ","token":"sentinel-token-a"}', session: 'sentinel-token-a' },
        // a non-string session must not shadow a usable token variant
        { body: '{"email":"a@b","session":123,"token":"sentinel-token-a"}', session: 'sentinel-token-a' },
      ];
      for (const { body, session } of cases) {
        const { request } = fakeSeam([body]);
        await expect(tryMintTestAccount(request, BASE)).resolves.toEqual({
          email: 'a@b',
          session,
          viaApi: true,
        });
      }
    });
  });

  test('missing/blank credentials stay setup-only: session undefined, never blank-string', async () => {
    await withEnvAsync(SEAM_ENV, async () => {
      const bodies = [
        '{"email":"a@b"}',
        '{"email":"a@b","session":""}',
        '{"email":"a@b","session":"   "}',
        '{"email":"a@b","token":""}',
        '{"email":"a@b","session":123}',
      ];
      for (const body of bodies) {
        const { request } = fakeSeam([body]);
        const account = await tryMintTestAccount(request, BASE);
        expect(account?.email).toBe('a@b');
        expect(account?.viaApi).toBe(true);
        expect(account?.session).toBeUndefined();
      }
    });
  });

  test('unconfigured seam resolves null without any request (UI-signup fallback preserved)', async () => {
    await withEnvAsync({ QA_AUTH_TOKEN: undefined }, async () => {
      const { request, posts } = fakeSeam([]);
      await expect(tryMintTestAccount(request, BASE)).resolves.toBeNull();
      expect(posts).toHaveLength(0);
    });
  });

  test('the seam setup request itself carries the QA token, requested identity and plan', async () => {
    await withEnvAsync(SEAM_ENV, async () => {
      const { request, posts } = fakeSeam([
        '{"email":"capped@example.test","session":"sentinel-session-capped"}',
      ]);
      const capped = await tryMintTestAccount(request, BASE, 'overlimit');
      expect(capped).toEqual({
        email: 'capped@example.test',
        session: 'sentinel-session-capped',
        viaApi: true,
      });
      expect(posts).toHaveLength(1);
      expect(posts[0]!.headers.Authorization).toBe(`Bearer ${QA_SENTINEL}`);
      expect(posts[0]!.data.plan).toBe('overlimit');
      expect(typeof posts[0]!.data.email).toBe('string');
      expect((posts[0]!.data.email as string).trim()).not.toBe('');
      expect(posts[0]!.maxRedirects).toBe(0);
    });
  });
});

test.describe('requireApiAccount account-identity guard', () => {
  test('passes with a nonblank minted session and no QA token', () => {
    withEnv({ QA_AUTH_TOKEN: undefined }, () => {
      expect(() => requireApiAccount(sessionAccount(), 'range')).not.toThrow();
    });
  });

  test('the QA seam token alone cannot satisfy the guard (null account)', () => {
    withEnv({ QA_AUTH_TOKEN: QA_SENTINEL }, () => {
      let message = '';
      try {
        requireApiAccount(null, 'meter');
      } catch (error) {
        message = (error as Error).message;
      }
      expect(message, 'QA token alone satisfied the account-identity guard').not.toBe('');
      expect(message).toMatch(/\[meter\]/);
      expect(message).not.toContain(QA_SENTINEL);
    });
  });

  test('a sessionless minted account cannot satisfy the guard, QA token or not', () => {
    withEnv({ QA_AUTH_TOKEN: QA_SENTINEL }, () => {
      expect(() =>
        requireApiAccount({ email: 'setup-only@example.test', viaApi: true }, 'range'),
      ).toThrow(/\[range\]/);
    });
  });

  test('a blank or whitespace-only session cannot satisfy the guard', () => {
    withEnv({ QA_AUTH_TOKEN: QA_SENTINEL }, () => {
      for (const session of ['', '   ']) {
        expect(() => requireApiAccount(sessionAccount({ session }), 'range')).toThrow(/\[range\]/);
      }
    });
  });
});

test.describe('accountHeaders use the minted credential only', () => {
  const headerEnv = {
    STAGING_BASE_URL: undefined,
    CF_ACCESS_CLIENT_ID: undefined,
    CF_ACCESS_CLIENT_SECRET: undefined,
    QA_AUTH_TOKEN: QA_SENTINEL,
  };

  test('session bearer is sent and the QA token appears nowhere', () => {
    withEnv(headerEnv, () => {
      const headers = accountHeaders(sessionAccount());
      expect(headers.Authorization).toBe('Bearer sentinel-session-a');
      expect(JSON.stringify(headers)).not.toContain(QA_SENTINEL);
    });
  });

  test('throws for a null account instead of falling back to the QA token', () => {
    withEnv(headerEnv, () => {
      let message = '';
      try {
        accountHeaders(null);
      } catch (error) {
        message = (error as Error).message;
      }
      expect(message, 'accountHeaders(null) returned QA-token headers').not.toBe('');
      expect(message).toMatch(/session|credential|minted/i);
      expect(message).not.toContain(QA_SENTINEL);
    });
  });

  test('throws for a sessionless or blank-session account', () => {
    withEnv(headerEnv, () => {
      expect(() => accountHeaders({ email: 'a@b', viaApi: true })).toThrow(
        /session|credential|minted/i,
      );
      for (const session of ['', '   ']) {
        expect(() => accountHeaders(sessionAccount({ session }))).toThrow(
          /session|credential|minted/i,
        );
      }
    });
  });
});

test.describe('two minted accounts stay distinct (incl. a capped account)', () => {
  test('paired fixtures: distinct identities, headers carry only the own session', async () => {
    await withEnvAsync(SEAM_ENV, async () => {
      const { request, posts } = fakeSeam([
        '{"email":"first@example.test","session":"sentinel-session-first"}',
        '{"email":"capped@example.test","session":"sentinel-session-capped"}',
      ]);
      const first = await tryMintTestAccount(request, BASE);
      const capped = await tryMintTestAccount(request, BASE, 'overlimit');
      expect(first).not.toBeNull();
      expect(capped).not.toBeNull();
      expect(first?.email).not.toBe(capped?.email);
      expect(first?.session).not.toBe(capped?.session);
      expect(posts[1]!.data.plan).toBe('overlimit');

      expect(() => requireApiAccount(first, 'range')).not.toThrow();
      expect(() => requireApiAccount(capped, 'plan-limit')).not.toThrow();

      const firstHeaders = accountHeaders(first);
      const cappedHeaders = accountHeaders(capped);
      expect(firstHeaders.Authorization).toBe('Bearer sentinel-session-first');
      expect(cappedHeaders.Authorization).toBe('Bearer sentinel-session-capped');
      expect(JSON.stringify(firstHeaders)).not.toContain('sentinel-session-capped');
      expect(JSON.stringify(cappedHeaders)).not.toContain('sentinel-session-first');
      expect(JSON.stringify(firstHeaders)).not.toContain(QA_SENTINEL);
      expect(JSON.stringify(cappedHeaders)).not.toContain(QA_SENTINEL);
    });
  });

  test('a malformed second mint cannot masquerade as the capped account', async () => {
    await withEnvAsync(SEAM_ENV, async () => {
      const { request } = fakeSeam(['{}']);
      await expect(tryMintTestAccount(request, BASE, 'overlimit')).rejects.toThrow(
        /\[seam\][\s\S]*(identity|email)/i,
      );
    });
  });
});

interface FakeRouteRecord {
  fetched?: { headers: Record<string, string>; maxRedirects?: number };
  fulfilled: boolean;
  continued: boolean;
}

function fakeRoute(url: string): { route: unknown; record: FakeRouteRecord } {
  const record: FakeRouteRecord = { fulfilled: false, continued: false };
  const route = {
    request: () => ({ url: () => url, headers: () => ({ 'x-original': '1' }) }),
    fetch: async (opts: { headers: Record<string, string>; maxRedirects?: number }) => {
      record.fetched = opts;
      return { fakeResponse: true };
    },
    fulfill: async () => {
      record.fulfilled = true;
    },
    continue: async () => {
      record.continued = true;
    },
  };
  return { route, record };
}

type RouteHandler = (route: unknown) => Promise<void>;

function fakePage(): { page: Page; handler: () => RouteHandler | undefined } {
  let routeHandler: RouteHandler | undefined;
  const context = {
    unrouteAll: async () => undefined,
    route: async (_pattern: string, handler: RouteHandler) => {
      routeHandler = handler;
    },
  };
  return {
    page: { context: () => context } as unknown as Page,
    handler: () => routeHandler,
  };
}

test.describe('applyAccountToPage browser credential scoping', () => {
  const pageEnv = {
    CF_ACCESS_CLIENT_ID: 'dummy-client-id',
    CF_ACCESS_CLIENT_SECRET: 'dummy-client-secret',
    QA_AUTH_TOKEN: QA_SENTINEL,
  };

  test('session account: staging origin gets its bearer (no QA token), foreign origin continues clean', async () => {
    await withEnvAsync(pageEnv, async () => {
      const { page, handler } = fakePage();
      await applyAccountToPage(page, sessionAccount(), BASE);
      const routeHandler = handler();
      expect(routeHandler, 'no route registered for a session account').toBeDefined();

      const staging = fakeRoute(`${BASE}/library`);
      await routeHandler!(staging.route);
      expect(staging.record.fetched?.headers.Authorization).toBe('Bearer sentinel-session-a');
      expect(staging.record.fetched?.maxRedirects).toBe(0);
      expect(JSON.stringify(staging.record.fetched?.headers)).not.toContain(QA_SENTINEL);
      expect(staging.record.fulfilled).toBe(true);

      const foreign = fakeRoute('https://foreign.example/asset');
      await routeHandler!(foreign.route);
      expect(foreign.record.continued).toBe(true);
      expect(foreign.record.fetched).toBeUndefined();
    });
  });

  test('null account stays Access-only (UI-signup fallback): no Authorization anywhere', async () => {
    await withEnvAsync(pageEnv, async () => {
      const { page, handler } = fakePage();
      await applyAccountToPage(page, null, BASE);
      const routeHandler = handler();
      expect(routeHandler, 'no Access-only route registered for null account').toBeDefined();

      const staging = fakeRoute(`${BASE}/signup`);
      await routeHandler!(staging.route);
      expect(staging.record.fetched?.headers.Authorization).toBeUndefined();
      expect(staging.record.fetched?.headers['CF-Access-Client-Id']).toBe('dummy-client-id');
      expect(JSON.stringify(staging.record.fetched?.headers)).not.toContain(QA_SENTINEL);
    });
  });

  test('a non-null sessionless account throws before any route is registered', async () => {
    await withEnvAsync(pageEnv, async () => {
      const { page, handler } = fakePage();
      await expect(
        applyAccountToPage(page, { email: 'setup-only@example.test', viaApi: true }, BASE),
      ).rejects.toThrow(/session|credential|minted/i);
      expect(handler()).toBeUndefined();
    });
  });

  test('a blank-session account throws before any route is registered', async () => {
    await withEnvAsync(pageEnv, async () => {
      const { page, handler } = fakePage();
      await expect(
        applyAccountToPage(page, sessionAccount({ session: '   ' }), BASE),
      ).rejects.toThrow(/session|credential|minted/i);
      expect(handler()).toBeUndefined();
    });
  });
});
