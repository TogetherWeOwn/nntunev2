import { test, expect, type APIRequestContext } from '@playwright/test';
import { createServer, type IncomingMessage, type Server } from 'node:http';
import type { AddressInfo } from 'node:net';
import { apiGet, apiPost } from '../helpers/accounts.js';

// Acceptance: credentialed redirect diagnostics must never echo URLs.
//
// Contract pinned here (implementation is a separate Builder card):
// - Ordinary-redirect and Cloudflare Access-wall errors from apiGet/apiPost
//   contain NO raw request-URL or Location components: no userinfo, host,
//   path, query or fragment from either URL, in plain or percent-encoded
//   form. Fixed route/context identifiers only.
// - Errors stay actionable: they retain the caller context, the failure
//   category, the HTTP status, and (Access wall) the CF_ACCESS_CLIENT_ID /
//   CF_ACCESS_CLIENT_SECRET variable NAMES without values.
// - Missing or malformed Location stays diagnosable (category + status +
//   a fixed location note) without echoing the header value.
// - No-follow is unchanged: maxRedirects 0, zero requests to a foreign
//   origin, credential header values never appear in the error.
//
// Fully offline: loopback fixture servers + a fake request context for
// request-URL strings a real HTTP stack cannot send. Sentinel values are
// synthetic and distinct per URL component so a leak names its source.

const SENT = {
  reqUserinfo: 'sentinel-req-userinfo-7f3a',
  reqPath: 'sentinel-req-path-2c9e',
  reqQuery: 'sentinel-req-query-9b1c',
  reqFragment: 'sentinel-req-frag-5d8a',
  reqEncoded: 'sentinel-req-encoded-41f0',
  locUserinfo: 'sentinel-loc-userinfo-83be',
  locTeam: 'sentinel-team-8e4f',
  locPath: 'sentinel-loc-path-6a27',
  locQuery: 'sentinel-loc-query-3f5d',
  locFragment: 'sentinel-loc-frag-90c4',
  locMalformed: 'sentinel-malformed-loc-4d2e',
  locMalformedQuery: 'sentinel-malformed-query-51aa',
  accessSecret: 'sentinel-access-secret-e771',
  accessId: 'sentinel-access-id-b204',
  session: 'sentinel-session-d619',
} as const;

const CONTEXT = 'diag-probe';

const credentialed = {
  'CF-Access-Client-Id': SENT.accessId,
  'CF-Access-Client-Secret': SENT.accessSecret,
  Authorization: `Bearer ${SENT.session}`,
};

function expectAbsent(message: string, sentinels: readonly string[]): void {
  for (const sentinel of sentinels) {
    expect(message, `redirect diagnostic leaked "${sentinel}"`).not.toContain(sentinel);
  }
}

async function errorMessage(fn: () => Promise<unknown>): Promise<string> {
  try {
    await fn();
  } catch (error) {
    return (error as Error).message;
  }
  throw new Error('expected the credentialed redirect to throw, but it resolved');
}

function expectActionableOrdinary(message: string, status: number): void {
  expect(message, 'diagnostic lost the caller context').toContain(CONTEXT);
  expect(message, 'diagnostic lost the redirect category').toMatch(/unexpected redirect/i);
  expect(message, 'diagnostic lost the HTTP status').toContain(String(status));
}

function expectActionableWall(message: string, status: number): void {
  expect(message, 'diagnostic lost the caller context').toContain(CONTEXT);
  expect(message, 'diagnostic lost the Access-wall category').toMatch(/Access wall/i);
  expect(message, 'diagnostic lost the HTTP status').toContain(String(status));
  expect(message, 'diagnostic lost the config variable names').toContain('CF_ACCESS_CLIENT_ID');
  expect(message, 'diagnostic lost the config variable names').toContain('CF_ACCESS_CLIENT_SECRET');
}

// ---------------------------------------------------------------------------
// Real Playwright request context against loopback fixture servers.
// ---------------------------------------------------------------------------

interface Fixture {
  stagingOrigin: string;
  foreignOrigin: string;
  stagingHits: Array<Record<string, string | string[] | undefined>>;
  foreignHits: Array<Record<string, string | string[] | undefined>>;
  close: () => Promise<void>;
}

async function startFixture(
  routes: Record<string, (foreignOrigin: string) => { status: number; location?: string }>,
): Promise<Fixture> {
  const listen = (server: Server): Promise<number> =>
    new Promise((resolve, reject) => {
      server.on('error', reject);
      server.listen(0, '127.0.0.1', () => resolve((server.address() as AddressInfo).port));
    });
  const closeServer = (server: Server): Promise<void> =>
    new Promise((resolve) => server.close(() => resolve()));

  const foreignHits: Fixture['foreignHits'] = [];
  const foreign = createServer((req: IncomingMessage, res) => {
    foreignHits.push({ ...req.headers });
    res.writeHead(200, { 'content-type': 'text/plain' });
    res.end('landed');
  });
  const foreignOrigin = `http://127.0.0.1:${await listen(foreign)}`;

  const stagingHits: Fixture['stagingHits'] = [];
  const staging = createServer((req: IncomingMessage, res) => {
    stagingHits.push({ ...req.headers });
    const pathname = (req.url ?? '').split(/[?#]/)[0] ?? '';
    for (const [prefix, respond] of Object.entries(routes)) {
      if (pathname.startsWith(prefix)) {
        const { status, location } = respond(foreignOrigin);
        res.writeHead(status, location === undefined ? {} : { location });
        res.end();
        return;
      }
    }
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end('{"ok":true}');
  });
  const stagingOrigin = `http://127.0.0.1:${await listen(staging)}`;

  return {
    stagingOrigin,
    foreignOrigin,
    stagingHits,
    foreignHits,
    close: async () => {
      await closeServer(staging).catch(() => {});
      await closeServer(foreign).catch(() => {});
    },
  };
}

function credentialValues(): string[] {
  return [SENT.accessSecret, SENT.accessId, SENT.session];
}

test.describe('redirect diagnostics redact URLs (real request)', () => {
  test('GET ordinary 302: no request/Location component or credential value in the error', async ({
    request,
  }) => {
    const fx = await startFixture({
      '/redir': (foreignOrigin) => ({
        status: 302,
        location:
          `http://${SENT.locUserinfo}@${foreignOrigin.slice('http://'.length)}` +
          `/${SENT.locPath}?next=${SENT.locQuery}#${SENT.locFragment}`,
      }),
    });
    try {
      const url =
        `http://${SENT.reqUserinfo}@${fx.stagingOrigin.slice('http://'.length)}` +
        `/redir/${SENT.reqPath}?token=${SENT.reqQuery}#${SENT.reqFragment}`;
      const message = await errorMessage(() => apiGet(request, url, credentialed, CONTEXT));

      expectActionableOrdinary(message, 302);
      expectAbsent(message, [
        SENT.reqUserinfo,
        SENT.reqPath,
        SENT.reqQuery,
        SENT.reqFragment,
        SENT.locUserinfo,
        SENT.locPath,
        SENT.locQuery,
        SENT.locFragment,
        fx.stagingOrigin,
        fx.foreignOrigin,
        '127.0.0.1',
        ...credentialValues(),
      ]);
      expect(fx.foreignHits, 'foreign origin received a follow-up request').toHaveLength(0);
    } finally {
      await fx.close();
    }
  });

  test('POST ordinary 307: no request/Location component or credential value in the error', async ({
    request,
  }) => {
    const fx = await startFixture({
      '/submit': (foreignOrigin) => ({
        status: 307,
        location:
          `http://${SENT.locUserinfo}@${foreignOrigin.slice('http://'.length)}` +
          `/${SENT.locPath}?next=${SENT.locQuery}#${SENT.locFragment}`,
      }),
    });
    try {
      const url =
        `${fx.stagingOrigin}/submit/${SENT.reqPath}?token=${SENT.reqQuery}#${SENT.reqFragment}`;
      const message = await errorMessage(() =>
        apiPost(request, url, credentialed, { email: 'a@b' }, CONTEXT),
      );

      expectActionableOrdinary(message, 307);
      expectAbsent(message, [
        SENT.reqPath,
        SENT.reqQuery,
        SENT.reqFragment,
        SENT.locUserinfo,
        SENT.locPath,
        SENT.locQuery,
        SENT.locFragment,
        fx.stagingOrigin,
        fx.foreignOrigin,
        '127.0.0.1',
        ...credentialValues(),
      ]);
      expect(fx.foreignHits, 'foreign origin received a follow-up request').toHaveLength(0);
    } finally {
      await fx.close();
    }
  });

  test('GET Access-wall 302: wall remediation without echoing either URL', async ({ request }) => {
    const wallLocation =
      `https://${SENT.locUserinfo}@${SENT.locTeam}.cloudflareaccess.com` +
      `/cdn-cgi/access/login/${SENT.locPath}?kid=${SENT.locQuery}#${SENT.locFragment}`;
    const fx = await startFixture({
      '/walled': () => ({ status: 302, location: wallLocation }),
    });
    try {
      const url = `${fx.stagingOrigin}/walled/${SENT.reqPath}?token=${SENT.reqQuery}#${SENT.reqFragment}`;
      const message = await errorMessage(() => apiGet(request, url, credentialed, CONTEXT));

      expectActionableWall(message, 302);
      expectAbsent(message, [
        SENT.reqPath,
        SENT.reqQuery,
        SENT.reqFragment,
        SENT.locUserinfo,
        SENT.locTeam,
        SENT.locPath,
        SENT.locQuery,
        SENT.locFragment,
        wallLocation,
        fx.stagingOrigin,
        '127.0.0.1',
        ...credentialValues(),
      ]);
      expect(fx.foreignHits, 'foreign origin received a follow-up request').toHaveLength(0);
    } finally {
      await fx.close();
    }
  });

  test('POST Access-wall 302: wall remediation without echoing either URL', async ({ request }) => {
    const wallLocation =
      `https://${SENT.locUserinfo}@${SENT.locTeam}.cloudflareaccess.com` +
      `/cdn-cgi/access/login/${SENT.locPath}?kid=${SENT.locQuery}#${SENT.locFragment}`;
    const fx = await startFixture({
      '/seam': () => ({ status: 302, location: wallLocation }),
    });
    try {
      const url = `${fx.stagingOrigin}/seam/${SENT.reqPath}?token=${SENT.reqQuery}#${SENT.reqFragment}`;
      const message = await errorMessage(() =>
        apiPost(request, url, credentialed, { email: 'a@b' }, CONTEXT),
      );

      expectActionableWall(message, 302);
      expectAbsent(message, [
        SENT.reqPath,
        SENT.reqQuery,
        SENT.reqFragment,
        SENT.locUserinfo,
        SENT.locTeam,
        SENT.locPath,
        SENT.locQuery,
        SENT.locFragment,
        wallLocation,
        fx.stagingOrigin,
        '127.0.0.1',
        ...credentialValues(),
      ]);
      expect(fx.foreignHits, 'foreign origin received a follow-up request').toHaveLength(0);
    } finally {
      await fx.close();
    }
  });

  for (const verb of ['GET', 'POST'] as const) {
    test(`${verb} 302 with no Location header: diagnosable, request URL not echoed`, async ({
      request,
    }) => {
      const fx = await startFixture({
        '/nolocation': () => ({ status: 302 }),
      });
      try {
        const url =
          `${fx.stagingOrigin}/nolocation/${SENT.reqPath}` +
          `?token=${SENT.reqQuery}#${SENT.reqFragment}`;
        const message = await errorMessage(() =>
          verb === 'GET'
            ? apiGet(request, url, credentialed, CONTEXT)
            : apiPost(request, url, credentialed, { email: 'a@b' }, CONTEXT),
        );

        expectActionableOrdinary(message, 302);
        expect(message, 'missing Location is not diagnosable from the error').toMatch(
          /no location|missing[^.]*location|location[^.]*(missing|absent)/i,
        );
        expectAbsent(message, [
          SENT.reqPath,
          SENT.reqQuery,
          SENT.reqFragment,
          fx.stagingOrigin,
          '127.0.0.1',
          ...credentialValues(),
        ]);
        expect(fx.foreignHits, 'foreign origin received a follow-up request').toHaveLength(0);
      } finally {
        await fx.close();
      }
    });
  }

  test('GET 302 with a malformed Location: diagnosable, header value not echoed', async ({
    request,
  }) => {
    const malformed = `:://${SENT.locMalformed} not a url ?q=${SENT.locMalformedQuery}`;
    const fx = await startFixture({
      '/badloc': () => ({ status: 302, location: malformed }),
    });
    try {
      const url = `${fx.stagingOrigin}/badloc/${SENT.reqPath}?token=${SENT.reqQuery}`;
      const message = await errorMessage(() => apiGet(request, url, credentialed, CONTEXT));

      expectActionableOrdinary(message, 302);
      expect(message, 'diagnostic lost the malformed-location note').toContain(
        'location malformed (redacted)',
      );
      expectAbsent(message, [
        SENT.locMalformed,
        SENT.locMalformedQuery,
        malformed,
        SENT.reqPath,
        SENT.reqQuery,
        fx.stagingOrigin,
        '127.0.0.1',
        ...credentialValues(),
      ]);
      expect(fx.foreignHits, 'foreign origin received a follow-up request').toHaveLength(0);
    } finally {
      await fx.close();
    }
  });

  test('positive control: non-redirect responses still pass through credentialed', async ({
    request,
  }) => {
    const fx = await startFixture({});
    try {
      const get = await apiGet(request, `${fx.stagingOrigin}/ok`, credentialed, CONTEXT);
      expect(get.status()).toBe(200);
      const post = await apiPost(
        request,
        `${fx.stagingOrigin}/ok`,
        credentialed,
        { email: 'a@b' },
        CONTEXT,
      );
      expect(post.status()).toBe(200);

      // Sanity: credentials really were attached, so the absence assertions
      // in the redirect tests above are meaningful.
      const staged = JSON.stringify(fx.stagingHits);
      expect(staged).toContain(SENT.accessSecret);
      expect(staged).toContain(SENT.session);
      expect(fx.foreignHits).toHaveLength(0);
    } finally {
      await fx.close();
    }
  });
});

// ---------------------------------------------------------------------------
// Fake request context: malformed request-URL strings (unfetchable for
// real) plus a direct pin on the maxRedirects: 0 fetch option.
// ---------------------------------------------------------------------------

interface CapturedCall {
  method: 'get' | 'post';
  url: string;
  opts: { maxRedirects?: number; headers?: Record<string, string> };
}

function fakeRequestContext(
  status: number,
  headers: Record<string, string>,
  calls: CapturedCall[],
): APIRequestContext {
  const respond = (method: 'get' | 'post') => async (url: string, opts: CapturedCall['opts']) => {
    calls.push({ method, url, opts: opts ?? {} });
    return { status: () => status, headers: () => headers };
  };
  return { get: respond('get'), post: respond('post') } as unknown as APIRequestContext;
}

test.describe('redirect diagnostics redact URLs (fake request context)', () => {
  test('GET malformed request URL: no component echoed, maxRedirects stays 0', async () => {
    const calls: CapturedCall[] = [];
    const location =
      `https://${SENT.locUserinfo}@foreign.example/${SENT.locPath}` +
      `?next=${SENT.locQuery}#${SENT.locFragment}`;
    const request = fakeRequestContext(302, { location }, calls);
    const malformedUrl =
      `ht tp://${SENT.reqUserinfo}@staging.example/${SENT.reqPath}` +
      `?token=${SENT.reqQuery}&next=%2F%2Fevil%3Fleak%3D${SENT.reqEncoded}#${SENT.reqFragment}`;

    const message = await errorMessage(() => apiGet(request, malformedUrl, credentialed, CONTEXT));

    expectActionableOrdinary(message, 302);
    expectAbsent(message, [
      SENT.reqUserinfo,
      SENT.reqPath,
      SENT.reqQuery,
      SENT.reqEncoded,
      SENT.reqFragment,
      'staging.example',
      SENT.locUserinfo,
      SENT.locPath,
      SENT.locQuery,
      SENT.locFragment,
      'foreign.example',
      ...credentialValues(),
    ]);
    expect(calls).toHaveLength(1);
    expect(calls[0]?.opts.maxRedirects, 'apiGet must never follow redirects').toBe(0);
  });

  test('POST malformed request URL at the Access wall: names only, maxRedirects stays 0', async () => {
    const calls: CapturedCall[] = [];
    const location =
      `https://${SENT.locUserinfo}@${SENT.locTeam}.cloudflareaccess.com` +
      `/cdn-cgi/access/login/${SENT.locPath}?kid=${SENT.locQuery}#${SENT.locFragment}`;
    const request = fakeRequestContext(302, { location }, calls);
    const malformedUrl =
      `ht tp://${SENT.reqUserinfo}@staging.example/${SENT.reqPath}` +
      `?token=${SENT.reqQuery}#${SENT.reqFragment}`;

    const message = await errorMessage(() =>
      apiPost(request, malformedUrl, credentialed, { email: 'a@b' }, CONTEXT),
    );

    expectActionableWall(message, 302);
    expectAbsent(message, [
      SENT.reqUserinfo,
      SENT.reqPath,
      SENT.reqQuery,
      SENT.reqFragment,
      'staging.example',
      SENT.locUserinfo,
      SENT.locTeam,
      SENT.locPath,
      SENT.locQuery,
      SENT.locFragment,
      ...credentialValues(),
    ]);
    expect(calls).toHaveLength(1);
    expect(calls[0]?.opts.maxRedirects, 'apiPost must never follow redirects').toBe(0);
  });
});
