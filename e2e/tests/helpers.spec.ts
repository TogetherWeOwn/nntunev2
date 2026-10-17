import { test, expect, chromium, type APIRequestContext } from '@playwright/test';
import { createServer, type IncomingMessage, type Server } from 'node:http';
import type { AddressInfo } from 'node:net';
import {
  accessHeaders,
  assertAccessWall,
  assertNoAccessWall,
  isAccessWall,
  isStagingOrigin,
  readApiBody,
  stagingBaseUrl,
  syntheticNntpEndpoint,
} from '../helpers/access.js';
import { extractPlanMeta, extractUsageBytes } from '../helpers/contract.js';
import {
  anonymousApiGet,
  apiGet,
  apiPost,
  applyAccountToPage,
  requireApiAccount,
  tryMintTestAccount,
} from '../helpers/accounts.js';

// Helper self-tests (pure functions only): no browser page, no staging.
// These run on every PR via e2e-lint AND inside the staging suite, pinning
// the fail-closed behavior — half-set Access secrets, non-https targets and
// Access-wall responses must never pass silent.
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

const dummyAccessPair = {
  CF_ACCESS_CLIENT_ID: 'dummy-client-id',
  CF_ACCESS_CLIENT_SECRET: 'dummy-client-secret',
};

const missingAccessPairs = [
  { name: 'both missing', CF_ACCESS_CLIENT_ID: undefined, CF_ACCESS_CLIENT_SECRET: undefined },
  { name: 'ID only', CF_ACCESS_CLIENT_ID: 'dummy-client-id', CF_ACCESS_CLIENT_SECRET: undefined },
  { name: 'secret only', CF_ACCESS_CLIENT_ID: undefined, CF_ACCESS_CLIENT_SECRET: 'dummy-client-secret' },
];

test.describe('accessHeaders', () => {
  test('empty when staging and the pair are unset (offline)', () => {
    withEnv({ STAGING_BASE_URL: undefined, CF_ACCESS_CLIENT_ID: undefined, CF_ACCESS_CLIENT_SECRET: undefined }, () => {
      expect(accessHeaders()).toEqual({});
    });
  });

  for (const { name, ...pair } of missingAccessPairs) {
    test(`configured staging rejects ${name} without echoing values`, () => {
      withEnv({ STAGING_BASE_URL: 'https://staging.example', ...pair }, () => {
        expect(() => accessHeaders()).toThrow(/CF_ACCESS_CLIENT_ID.*CF_ACCESS_CLIENT_SECRET/);
        try {
          accessHeaders();
        } catch (error) {
          expect((error as Error).message).not.toContain('dummy-client-id');
          expect((error as Error).message).not.toContain('dummy-client-secret');
        }
      });
    });
  }

  test('complete pair supplies the required headers', () => {
    withEnv({ STAGING_BASE_URL: 'https://staging.example', ...dummyAccessPair }, () => {
      expect(accessHeaders()).toEqual({
        'CF-Access-Client-Id': dummyAccessPair.CF_ACCESS_CLIENT_ID,
        'CF-Access-Client-Secret': dummyAccessPair.CF_ACCESS_CLIENT_SECRET,
      });
    });
  });

  test('fail-closed when only half the pair is set', () => {
    withEnv({ CF_ACCESS_CLIENT_ID: 'id-only', CF_ACCESS_CLIENT_SECRET: undefined }, () => {
      expect(() => accessHeaders()).toThrow(/misconfigured/);
    });
  });

  test('never echoes a secret value in the throw', () => {
    withEnv({ CF_ACCESS_CLIENT_ID: 'id-only', CF_ACCESS_CLIENT_SECRET: undefined }, () => {
      let message = '';
      try {
        accessHeaders();
      } catch (error) {
        message = (error as Error).message;
      }
      expect(message).toMatch(/misconfigured/);
      expect(message).not.toContain('id-only');
    });
  });
});

test.describe('assertNoAccessWall', () => {
  test('throws on a 403 with Access markers', () => {
    expect(() =>
      assertNoAccessWall(
        {
          status: 403,
          headers: { 'cf-mitigated': 'challenge' },
          bodyText: '<title>Access | Cloudflare</title>',
          url: 'https://staging.example/api/health',
        },
        'health',
      ),
    ).toThrow(/Access wall/);
  });

  test('403 wall error never echoes the probe URL (host, path, query, fragment)', () => {
    const url =
      'https://sentinel-staging-host.example/sentinel-wall-path?token=sentinel-wall-query#sentinel-wall-frag';
    let message = '';
    try {
      assertNoAccessWall(
        {
          status: 403,
          headers: { 'cf-mitigated': 'challenge' },
          bodyText: '<title>Access | Cloudflare</title>',
          url,
        },
        'health',
      );
    } catch (error) {
      message = (error as Error).message;
    }
    expect(message).toContain('health');
    expect(message).toMatch(/Access wall/);
    expect(message).toContain('403');
    expect(message).toContain('CF_ACCESS_CLIENT_ID');
    expect(message).toContain('CF_ACCESS_CLIENT_SECRET');
    for (const sentinel of [
      url,
      'sentinel-staging-host.example',
      'sentinel-wall-path',
      'sentinel-wall-query',
      'sentinel-wall-frag',
    ]) {
      expect(message, `Access-wall diagnostic leaked "${sentinel}"`).not.toContain(sentinel);
    }
  });

  test('passes a normal app response through', () => {
    expect(() =>
      assertNoAccessWall(
        {
          status: 200,
          headers: { 'content-type': 'application/json' },
          bodyText: '{"ok":true}',
          url: 'x',
        },
        'health',
      ),
    ).not.toThrow();
  });

  test('a legitimate app 403 without markers is NOT a wall', () => {
    expect(() =>
      assertNoAccessWall(
        {
          status: 403,
          headers: { 'content-type': 'application/json' },
          bodyText: '{"error":"plan_limit_exceeded"}',
          url: 'https://staging.example/api/stream/abc',
        },
        'plan-limit',
      ),
    ).not.toThrow();
  });

  test('a 302 to the Access login is a wall', () => {
    expect(() =>
      assertNoAccessWall(
        {
          status: 302,
          headers: { location: 'https://example.cloudflareaccess.com/cdn-cgi/access/login' },
          bodyText: '',
          url: 'https://staging.example/api/usage',
        },
        'meter',
      ),
    ).toThrow(/Access wall/);
  });

  test('a plain app 302 without markers is NOT a wall', () => {
    expect(() =>
      assertNoAccessWall(
        {
          status: 302,
          headers: { location: '/login' },
          bodyText: '',
          url: 'https://staging.example/library',
        },
        'app-shell',
      ),
    ).not.toThrow();
  });
});

test.describe('isAccessWall / assertAccessWall (anonymous denial)', () => {
  const wall403 = {
    status: 403,
    headers: { 'cf-mitigated': 'challenge' },
    bodyText: '<title>Access | Cloudflare</title>',
    url: 'https://staging.example/api/health',
  };
  const wall302 = {
    status: 302,
    headers: { location: 'https://example.cloudflareaccess.com/cdn-cgi/access/login' },
    bodyText: '',
    url: 'https://staging.example/api/ready',
  };

  test('recognises Access-specific denials (403 wall and Access-login 302)', () => {
    expect(isAccessWall(wall403)).toBe(true);
    expect(isAccessWall(wall302)).toBe(true);
    expect(() => assertAccessWall(wall403, 'health-anonymous')).not.toThrow();
    expect(() => assertAccessWall(wall302, 'ready-anonymous')).not.toThrow();
  });

  test('an app ok:true answer without Access is a failure, reported without echoing url or body', () => {
    const probe = {
      status: 200,
      headers: { 'content-type': 'application/json' },
      bodyText: '{"ok":true,"note":"sentinel-body"}',
      url: 'https://sentinel-staging-host.example/sentinel-path?token=sentinel-query#sentinel-frag',
    };
    expect(isAccessWall(probe)).toBe(false);
    let message = '';
    try {
      assertAccessWall(probe, 'health-anonymous');
    } catch (error) {
      message = (error as Error).message;
    }
    expect(message).toContain('health-anonymous');
    expect(message).toContain('200');
    expect(message).toMatch(/expected Cloudflare Access denial/);
    for (const sentinel of [
      probe.url,
      'sentinel-staging-host.example',
      'sentinel-path',
      'sentinel-query',
      'sentinel-frag',
      'sentinel-body',
    ]) {
      expect(message, `anonymous-denial diagnostic leaked "${sentinel}"`).not.toContain(sentinel);
    }
  });

  test('a plain app 403 without markers is NOT Access-specific denial', () => {
    expect(() =>
      assertAccessWall(
        {
          status: 403,
          headers: { 'content-type': 'application/json' },
          bodyText: '{"error":"plan_limit_exceeded"}',
          url: 'https://staging.example/api/health',
        },
        'health-anonymous',
      ),
    ).toThrow(/expected Cloudflare Access denial/);
  });

  test('a plain app 302 without markers is NOT Access-specific denial', () => {
    expect(() =>
      assertAccessWall(
        {
          status: 302,
          headers: { location: '/login' },
          bodyText: '',
          url: 'https://staging.example/api/ready',
        },
        'ready-anonymous',
      ),
    ).toThrow(/expected Cloudflare Access denial/);
  });
});

test.describe('anonymousApiGet (isolated, credential-free, no-follow)', () => {
  test('sends no Access/bearer/cookie headers and returns wall probes unfollowed', async () => {
    const listen = (server: Server): Promise<number> =>
      new Promise((resolve, reject) => {
        server.on('error', reject);
        server.listen(0, '127.0.0.1', () => resolve((server.address() as AddressInfo).port));
      });
    const close = (server: Server): Promise<void> =>
      new Promise((resolve) => server.close(() => resolve()));

    const hits: Array<Record<string, string | string[] | undefined>> = [];
    const staging = createServer((req: IncomingMessage, res) => {
      hits.push({ ...req.headers });
      if (req.url === '/walled') {
        res.writeHead(403, { 'cf-mitigated': 'challenge', 'content-type': 'text/html' });
        res.end('<title>Access | Cloudflare</title>');
        return;
      }
      if (req.url === '/redir-access') {
        res.writeHead(302, {
          location: 'https://example.cloudflareaccess.com/cdn-cgi/access/login',
        });
        res.end();
        return;
      }
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end('{"ok":true}');
    });
    const port = await listen(staging);
    const origin = `http://127.0.0.1:${port}`;

    try {
      // Even with a complete pair and seam token in the environment, the
      // anonymous probe must attach none of them.
      await withEnvAsync(
        { ...dummyAccessPair, QA_AUTH_TOKEN: 'dummy-qa-token-anon-probe' },
        async () => {
          const walled = await anonymousApiGet(`${origin}/walled`, 'health-anonymous');
          expect(walled.status).toBe(403);
          expect(() => assertAccessWall(walled, 'health-anonymous')).not.toThrow();

          // Returned as the 302 itself: a followed Access login could not
          // surface as this probe.
          const redir = await anonymousApiGet(`${origin}/redir-access`, 'ready-anonymous');
          expect(redir.status).toBe(302);
          expect(() => assertAccessWall(redir, 'ready-anonymous')).not.toThrow();

          const ok = await anonymousApiGet(`${origin}/ok`, 'health-anonymous');
          expect(ok.status).toBe(200);
          expect(() => assertAccessWall(ok, 'health-anonymous')).toThrow(
            /expected Cloudflare Access denial/,
          );
        },
      );

      expect(hits).toHaveLength(3);
      for (const headers of hits) {
        expect(headers['cf-access-client-id']).toBeUndefined();
        expect(headers['cf-access-client-secret']).toBeUndefined();
        expect(headers.authorization).toBeUndefined();
        expect(headers.cookie).toBeUndefined();
      }
      const dumped = JSON.stringify(hits);
      expect(dumped).not.toContain('dummy-client-id');
      expect(dumped).not.toContain('dummy-client-secret');
      expect(dumped).not.toContain('dummy-qa-token-anon-probe');
    } finally {
      await close(staging).catch(() => {});
    }
  });
});

test.describe('readApiBody', () => {
  test('returns the body text for a clean response', async () => {
    const body = await readApiBody(
      {
        status: () => 200,
        headers: () => ({ 'content-type': 'application/json' }),
        text: async () => '{"ok":true}',
      },
      'https://staging.example/api/health',
      'health',
    );
    expect(body).toBe('{"ok":true}');
  });

  test('throws on a walled response instead of returning it', async () => {
    await expect(
      readApiBody(
        {
          status: () => 403,
          headers: () => ({ 'cf-mitigated': 'challenge' }),
          text: async () => '<title>Access | Cloudflare</title>',
        },
        'https://staging.example/api/library',
        'range',
      ),
    ).rejects.toThrow(/Access wall/);
  });
});

test.describe('stagingBaseUrl', () => {
  test('null (skip) when unconfigured', () => {
    withEnv({ STAGING_BASE_URL: undefined }, () => {
      expect(stagingBaseUrl('skip', 'health')).toBeNull();
    });
  });

  test('fail-closed when required but unconfigured', () => {
    withEnv({ STAGING_BASE_URL: undefined }, () => {
      expect(() => stagingBaseUrl('fail-closed', 'health')).toThrow(/STAGING_BASE_URL/);
    });
  });

  test('rejects non-https targets', () => {
    withEnv({ STAGING_BASE_URL: 'http://staging.example' }, () => {
      expect(() => stagingBaseUrl('skip', 'health')).toThrow(/https/);
    });
  });

  for (const { name, ...pair } of missingAccessPairs) {
    test(`URL resolution rejects ${name} before requests`, () => {
      withEnv({ STAGING_BASE_URL: 'https://staging.example', ...pair }, () => {
        expect(() => stagingBaseUrl('skip', 'health')).toThrow(/CF_ACCESS_CLIENT_ID/);
        expect(() => stagingBaseUrl('fail-closed', 'journey')).toThrow(/CF_ACCESS_CLIENT_SECRET/);
      });
    });
  }

  test('strips trailing slashes with a complete Access pair', () => {
    withEnv({ STAGING_BASE_URL: 'https://staging.example///', ...dummyAccessPair }, () => {
      expect(stagingBaseUrl('skip', 'health')).toBe('https://staging.example');
    });
  });
});

test.describe('syntheticNntpEndpoint', () => {
  test('rejects an invalid port', () => {
    withEnv({ SYNTHETIC_NNTP_PORT: 'notaport' }, () => {
      expect(() => syntheticNntpEndpoint()).toThrow(/PORT/);
    });
  });

  test('reports password presence without exposing it', () => {
    withEnv({ SYNTHETIC_NNTP_PASS: 's3cret-value' }, () => {
      const endpoint = syntheticNntpEndpoint();
      expect(endpoint.hasPassword).toBe(true);
      expect(JSON.stringify(endpoint)).not.toContain('s3cret-value');
    });
  });
});

test.describe('extractUsageBytes', () => {
  test('reads the canonical field', () => {
    expect(extractUsageBytes({ bytes_streamed: 41 })).toBe(41);
  });

  test('reads one level of nesting', () => {
    expect(extractUsageBytes({ usage: { used_bytes: 7 } })).toBe(7);
  });

  test('names the actual keys on contract drift', () => {
    expect(() => extractUsageBytes({ renamed_field: 1 })).toThrow(/renamed_field/);
  });
});

test.describe('extractPlanMeta', () => {
  test('reads plan metadata', () => {
    expect(extractPlanMeta({ limit_bytes: 100, used_bytes: 3, plan: 'base' })).toEqual({
      limit: 100,
      used: 3,
      tier: 'base',
    });
  });

  test('throws when metadata is incomplete', () => {
    expect(() => extractPlanMeta({ plan: 'base' })).toThrow(/incomplete/);
  });
});

test.describe('isStagingOrigin (credential scope gate)', () => {
  const staging = 'https://staging.example';

  test('attaches to same-origin paths', () => {
    expect(isStagingOrigin('https://staging.example/api/usage', staging)).toBe(true);
    expect(isStagingOrigin('https://staging.example/library', staging)).toBe(true);
  });

  test('withholds from a foreign origin', () => {
    expect(isStagingOrigin('https://fonts.example/css', staging)).toBe(false);
    expect(isStagingOrigin('https://cdn.example/stream/1', staging)).toBe(false);
  });

  test('withholds from a subdomain lookalike', () => {
    expect(isStagingOrigin('https://staging.example.evil.test/x', staging)).toBe(false);
  });

  test('withholds on unparseable URLs', () => {
    expect(isStagingOrigin('not a url', staging)).toBe(false);
  });

  test('two-origin simulation: the foreign origin sees neither header', () => {
    const credentialed = {
      'CF-Access-Client-Id': 'dummy-client-id',
      'CF-Access-Client-Secret': 'dummy-client-secret',
      Authorization: 'Bearer dummy-session',
    };
    const send = (url: string): Record<string, string> =>
      isStagingOrigin(url, staging) ? credentialed : {};
    expect(send('https://staging.example/library')).toEqual(credentialed);
    const foreign = send('https://cdn.example/stream/1');
    expect(foreign).toEqual({});
    expect(JSON.stringify(foreign)).not.toContain('dummy-client-secret');
    expect(JSON.stringify(foreign)).not.toContain('dummy-session');
  });
});

test.describe('applyAccountToPage redirect scope (browser)', () => {
  // Real Chromium repro for the redirect leak: staging answers /redir with a
  // 302 to a foreign origin. route.continue({ headers }) keeps the headers on
  // the automatic follow; fetch(maxRedirects: 0) + fulfill must not.
  test('staging 302 to a foreign origin carries no credentials', async () => {
    // Launch manually (not via the browser fixture) so containers without
    // system browser libs skip with notice instead of failing setup; CI
    // installs --with-deps so this runs for real there.
    let browser: Awaited<ReturnType<typeof chromium.launch>> | undefined;
    try {
      browser = await chromium.launch();
    } catch (error) {
      test.skip(true, `Chromium unavailable in this container (${(error as Error).message.split('\n')[0]}); runs in CI with --with-deps.`);
      return;
    }
    const markerSecret = 'dummy-client-secret-redirect-probe';
    const markerSession = 'dummy-session-redirect-probe';
    const markerId = 'dummy-client-id-redirect-probe';

    const listen = (server: Server): Promise<number> =>
      new Promise((resolve, reject) => {
        server.on('error', reject);
        server.listen(0, '127.0.0.1', () => resolve((server.address() as AddressInfo).port));
      });
    const close = (server: Server): Promise<void> =>
      new Promise((resolve) => server.close(() => resolve()));

    let foreignOrigin = '';
    const foreignHits: Array<Record<string, string | string[] | undefined>> = [];
    const foreign = createServer((req: IncomingMessage, res) => {
      foreignHits.push({ ...req.headers });
      res.writeHead(200, { 'content-type': 'text/plain' });
      res.end('landed');
    });
    const foreignPort = await listen(foreign);
    foreignOrigin = `http://127.0.0.1:${foreignPort}`;

    const stagingHits: Array<Record<string, string | string[] | undefined>> = [];
    const staging = createServer((req: IncomingMessage, res) => {
      stagingHits.push({ ...req.headers });
      if (req.url === '/redir') {
        res.writeHead(302, { location: `${foreignOrigin}/landed` });
        res.end();
        return;
      }
      res.writeHead(200, { 'content-type': 'text/plain' });
      res.end('staging');
    });
    const stagingPort = await listen(staging);
    const stagingOrigin = `http://127.0.0.1:${stagingPort}`;

    const activeBrowser = browser;
    try {
      await withEnvAsync(
        { CF_ACCESS_CLIENT_ID: markerId, CF_ACCESS_CLIENT_SECRET: markerSecret },
        async () => {
          const context = await activeBrowser.newContext();
          try {
            const page = await context.newPage();
            await applyAccountToPage(
              page,
              { email: 'e2e@example.test', session: markerSession, viaApi: true },
              stagingOrigin,
            );
            await page.goto(`${stagingOrigin}/redir`, { waitUntil: 'domcontentloaded' });
            await expect(page.getByText('landed')).toBeVisible({ timeout: 15_000 });
          } finally {
            await context.close();
          }
        },
      );

      // Sanity: staging itself received the credentials (the test attached them).
      expect(stagingHits.length).toBeGreaterThan(0);
      const staged = JSON.stringify(stagingHits);
      expect(staged).toContain(markerSecret);
      expect(staged).toContain(markerSession);

      // The foreign origin after the redirect must see neither header.
      expect(foreignHits.length).toBeGreaterThan(0);
      for (const headers of foreignHits) {
        expect(headers['cf-access-client-id']).toBeUndefined();
        expect(headers['cf-access-client-secret']).toBeUndefined();
        expect(headers.authorization).toBeUndefined();
      }
      const dumped = JSON.stringify(foreignHits);
      expect(dumped).not.toContain(markerSecret);
      expect(dumped).not.toContain(markerSession);
      expect(dumped).not.toContain(markerId);
    } finally {
      await close(staging).catch(() => {});
      await close(foreign).catch(() => {});
      await activeBrowser.close().catch(() => {});
    }
  });
});

test.describe('apiGet/apiPost redirect scope (no-follow)', () => {
  // API repro for the redirect leak: Playwright's APIRequestContext follows
  // redirects by default and forwards custom headers to the target. apiGet/
  // apiPost fetch with maxRedirects: 0 so a staging 302 to another origin is
  // returned as-is (foreign origin receives zero requests) and the 3xx fails
  // by name instead of a bare status mismatch.
  test('credentialed 302 to a foreign origin is not followed and fails by name', async ({
    request,
  }) => {
    const markerSecret = 'dummy-client-secret-api-probe';
    const markerSession = 'dummy-session-api-probe';
    const markerId = 'dummy-client-id-api-probe';
    const credentialed = {
      'CF-Access-Client-Id': markerId,
      'CF-Access-Client-Secret': markerSecret,
      Authorization: `Bearer ${markerSession}`,
    };

    const listen = (server: Server): Promise<number> =>
      new Promise((resolve, reject) => {
        server.on('error', reject);
        server.listen(0, '127.0.0.1', () => resolve((server.address() as AddressInfo).port));
      });
    const close = (server: Server): Promise<void> =>
      new Promise((resolve) => server.close(() => resolve()));

    const foreignHits: Array<Record<string, string | string[] | undefined>> = [];
    const foreign = createServer((req: IncomingMessage, res) => {
      foreignHits.push({ ...req.headers });
      res.writeHead(200, { 'content-type': 'text/plain' });
      res.end('landed');
    });
    const foreignPort = await listen(foreign);
    const foreignOrigin = `http://127.0.0.1:${foreignPort}`;

    const staging = createServer((req: IncomingMessage, res) => {
      if (req.url === '/redir') {
        res.writeHead(302, { location: `${foreignOrigin}/landed` });
        res.end();
        return;
      }
      if (req.url === '/submit') {
        res.writeHead(302, { location: `${foreignOrigin}/landed` });
        res.end();
        return;
      }
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end('{"ok":true}');
    });
    const stagingPort = await listen(staging);
    const stagingOrigin = `http://127.0.0.1:${stagingPort}`;

    try {
      // Sanity: a non-redirect passes through with its body.
      const ok = await apiGet(request, `${stagingOrigin}/ok`, credentialed, 'probe');
      expect(ok.status()).toBe(200);

      // GET 302 must throw naming the redirect; the foreign origin sees nothing.
      await expect(
        apiGet(request, `${stagingOrigin}/redir`, credentialed, 'probe'),
        'apiGet followed a cross-origin 302 with credentials instead of failing',
      ).rejects.toThrow(/unexpected redirect.*HTTP 302/);
      expect(foreignHits, 'foreign origin received a credentialed follow-up to GET 302').toHaveLength(
        0,
      );

      // POST 302 (the seam path) behaves the same.
      await expect(
        apiPost(request, `${stagingOrigin}/submit`, credentialed, { email: 'a@b' }, 'probe'),
        'apiPost followed a cross-origin 302 with credentials instead of failing',
      ).rejects.toThrow(/unexpected redirect.*HTTP 302/);
      expect(foreignHits, 'foreign origin received a credentialed follow-up to POST 302').toHaveLength(
        0,
      );

      // No credential value reached the foreign origin in any form.
      expect(JSON.stringify(foreignHits)).not.toContain(markerSecret);
      expect(JSON.stringify(foreignHits)).not.toContain(markerSession);
      expect(JSON.stringify(foreignHits)).not.toContain(markerId);
    } finally {
      await close(staging).catch(() => {});
      await close(foreign).catch(() => {});
    }
  });
});

test.describe('requireApiAccount', () => {
  test('throws when no session and no seam token exist', () => {
    withEnv({ QA_AUTH_TOKEN: undefined }, () => {
      expect(() => requireApiAccount(null, 'range')).toThrow(/no authenticated account/);
    });
  });

  test('passes with a minted session', () => {
    withEnv({ QA_AUTH_TOKEN: undefined }, () => {
      expect(() =>
        requireApiAccount({ email: 'e2e@example.test', session: 's', viaApi: true }, 'range'),
      ).not.toThrow();
    });
  });

  // Updated expectation (account-identity contract, tests/account-identity.spec.ts):
  // the generic QA seam token is setup-only and must NOT satisfy the
  // account-identity guard — account-specific assertions need the minted
  // account's own nonblank session.
  test('throws on the seam token alone (QA token cannot satisfy the guard)', () => {
    withEnv({ QA_AUTH_TOKEN: 'dummy-qa-token' }, () => {
      expect(() => requireApiAccount(null, 'meter')).toThrow(/\[meter\]/);
    });
  });
});

test.describe('tryMintTestAccount fail-closed seam', () => {
  const base = 'https://staging.example';
  const seamEnv = {
    STAGING_BASE_URL: base,
    QA_AUTH_TOKEN: 'dummy-qa-token',
    ...dummyAccessPair,
  };
  const fakeRequest = (post: () => Promise<unknown>): APIRequestContext =>
    ({ post }) as unknown as APIRequestContext;
  const fakeResponse = (status: number, headers: Record<string, string>, body: string) => ({
    status: () => status,
    headers: () => headers,
    text: async () => body,
  });

  test('null when the seam is unconfigured (UI signup fallback)', async () => {
    await withEnvAsync({ QA_AUTH_TOKEN: undefined }, async () => {
      await expect(
        tryMintTestAccount(
          fakeRequest(async () => {
            throw new Error('must not be called without a token');
          }),
          base,
        ),
      ).resolves.toBeNull();
    });
  });

  test('throws on transport failure once the token is set', async () => {
    await withEnvAsync(seamEnv, async () => {
      await expect(
        tryMintTestAccount(
          fakeRequest(async () => {
            throw new Error('boom');
          }),
          base,
        ),
      ).rejects.toThrow(/transport/);
    });
  });

  test('throws with the bare status on non-2xx (no silent skip)', async () => {
    await withEnvAsync(seamEnv, async () => {
      await expect(
        tryMintTestAccount(fakeRequest(async () => fakeResponse(500, {}, '{"error":"x"}')), base),
      ).rejects.toThrow(/HTTP 500/);
    });
  });

  test('names the Access wall on walled seam responses', async () => {
    await withEnvAsync(seamEnv, async () => {
      await expect(
        tryMintTestAccount(
          fakeRequest(
            async () => fakeResponse(403, { 'cf-mitigated': 'challenge' }, '<title>Access</title>'),
          ),
          base,
        ),
      ).rejects.toThrow(/Access wall/);
    });
  });

  test('walled seam error never echoes the staging host', async () => {
    await withEnvAsync(seamEnv, async () => {
      let message = '';
      try {
        await tryMintTestAccount(
          fakeRequest(
            async () => fakeResponse(403, { 'cf-mitigated': 'challenge' }, '<title>Access</title>'),
          ),
          'https://sentinel-staging-host.example',
        );
      } catch (error) {
        message = (error as Error).message;
      }
      expect(message).toMatch(/Access wall/);
      expect(message).not.toContain('sentinel-staging-host.example');
    });
  });

  test('throws on a non-JSON body', async () => {
    await withEnvAsync(seamEnv, async () => {
      await expect(
        tryMintTestAccount(fakeRequest(async () => fakeResponse(200, {}, 'not json')), base),
      ).rejects.toThrow(/non-JSON/);
    });
  });

  test('mints from session and legacy token fields', async () => {
    await withEnvAsync(seamEnv, async () => {
      await expect(
        tryMintTestAccount(
          fakeRequest(async () => fakeResponse(200, {}, '{"email":"a@b","session":"s"}')),
          base,
        ),
      ).resolves.toEqual({ email: 'a@b', session: 's', viaApi: true });
      await expect(
        tryMintTestAccount(
          fakeRequest(async () => fakeResponse(200, {}, '{"email":"a@b","token":"t"}')),
          base,
        ),
      ).resolves.toEqual({ email: 'a@b', session: 't', viaApi: true });
    });
  });
});

test.describe('redirect location-state diagnostics (value-free)', () => {
  // Pins the malformed/present notes from locationNote via a fake request
  // context: a regression collapsing malformed into present (or dropping
  // the whitespace/control guard) must fail here, and neither state may
  // echo the header value.
  const fakeRedirect = (location?: string): APIRequestContext =>
    ({
      get: async () => ({
        status: () => 302,
        headers: () => (location === undefined ? {} : { location }),
      }),
    }) as unknown as APIRequestContext;

  const redirectMessage = async (location?: string): Promise<string> => {
    try {
      await apiGet(fakeRedirect(location), 'https://staging.example/probe', {}, 'probe');
    } catch (error) {
      return (error as Error).message;
    }
    throw new Error('expected the credentialed redirect to throw, but it resolved');
  };

  test('malformed Location is reported as malformed, value not echoed', async () => {
    const malformed = ':://sentinel-malformed-loc not a url ?q=sentinel-malformed-query';
    const message = await redirectMessage(malformed);
    expect(message).toMatch(/unexpected redirect/i);
    expect(message).toContain('302');
    expect(message).toMatch(/malformed/i);
    expect(message).not.toMatch(/location present/i);
    expect(message).not.toContain('sentinel-malformed-loc');
    expect(message).not.toContain('sentinel-malformed-query');
    expect(message).not.toContain(malformed);
  });

  test('whitespace-bearing Location is malformed even though URL parsing would tolerate it', async () => {
    // `new URL('/landed path', base)` percent-encodes and succeeds, so only
    // the whitespace/control guard classifies this as malformed.
    const message = await redirectMessage('/landed path?next=sentinel-space-query');
    expect(message).toMatch(/malformed/i);
    expect(message).not.toMatch(/location present/i);
    expect(message).not.toContain('sentinel-space-query');
    expect(message).not.toContain('landed path');
  });

  test('well-formed Location is reported as present, value not echoed', async () => {
    const message = await redirectMessage(
      'https://foreign.example/sentinel-loc-path?next=sentinel-loc-query#sentinel-loc-frag',
    );
    expect(message).toMatch(/unexpected redirect/i);
    expect(message).toContain('302');
    expect(message).toMatch(/location present/i);
    expect(message).not.toMatch(/malformed/i);
    expect(message).not.toContain('foreign.example');
    expect(message).not.toContain('sentinel-loc-path');
    expect(message).not.toContain('sentinel-loc-query');
    expect(message).not.toContain('sentinel-loc-frag');
  });

  test('relative Location is present, not malformed', async () => {
    const message = await redirectMessage('/library?next=sentinel-rel-query');
    expect(message).toMatch(/location present/i);
    expect(message).not.toMatch(/malformed/i);
    expect(message).not.toContain('sentinel-rel-query');
  });
});
