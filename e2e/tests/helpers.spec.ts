import { test, expect, chromium, type APIRequestContext } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { createServer, type IncomingMessage, type Server } from 'node:http';
import type { AddressInfo } from 'node:net';
import {
  accessHeaders,
  assertAccessWall,
  assertNoAccessWall,
  isAccessWall,
  isAuthStartRequest,
  isStagingOrigin,
  readApiBody,
  stagingBaseUrl,
  syntheticNntpEndpoint,
} from '../helpers/access.js';
import { assertPlanLimitHeaders, assertPlanLimitRefusal, extractLibraryItems, extractLimitCode, extractPlanMeta, extractRecordingMbids, extractUpgradeCta, extractUsageBytes, isMbid, secretLeak } from '../helpers/contract.js';
import {
  anonymousApiGet,
  anonymousApiOptions,
  anonymousGet,
  annotateSignupPath,
  apiGet,
  apiOptions,
  apiPost,
  applyAccountToPage,
  requireApiAccount,
  signupPathLabel,
  tryMintTestAccount,
  awaitSignupOutcome,
  recordRegisterStartStatus,
  signupInlineErrorMessage,
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
          bodyText: '<title>Access | Cloudflare Access</title>',
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
          bodyText: '<title>Access | Cloudflare Access</title>',
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
    bodyText: '<title>Access | Cloudflare Access</title>',
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

  test('a generic Cloudflare challenge without Access markers is NOT an Access denial', () => {
    // Regression pin: `cf-mitigated` / `cf_clearance` are generic
    // edge-challenge markers, not Access-specific. A 403 carrying only
    // them must not pass as proof of Access enforcement.
    const probe = {
      status: 403,
      headers: { 'cf-mitigated': 'challenge' },
      bodyText: '<title>Just a moment...</title>',
      url: 'https://staging.example/api/health',
    };
    expect(isAccessWall(probe)).toBe(false);
    expect(() => assertAccessWall(probe, 'health-anonymous')).toThrow(
      /expected Cloudflare Access denial/,
    );
  });

  test('an app 403 whose header VALUE mentions Access is NOT an Access denial', () => {
    // Regression pin: markers match header names only. An ordinary app 403
    // echoing `CF-Access-Client-Id` in a header value (e.g. CORS
    // allow-headers) must not pass as proof of Access enforcement.
    const probe = {
      status: 403,
      headers: { 'access-control-allow-headers': 'CF-Access-Client-Id' },
      bodyText: '{"error":"forbidden"}',
      url: 'https://staging.example/api/health',
    };
    expect(isAccessWall(probe)).toBe(false);
    expect(() => assertAccessWall(probe, 'health-anonymous')).toThrow(
      /expected Cloudflare Access denial/,
    );
  });

  test('a generic 403 title without an Access marker is NOT an Access denial', () => {
    // Regression pin: a generic `<title>Access denied...` page proves
    // nothing about Cloudflare Access. Only an explicit `Cloudflare
    // Access` marker passes as proof of enforcement.
    const probe = {
      status: 403,
      headers: { 'content-type': 'text/html' },
      bodyText: '<title>Access denied | example.com</title>',
      url: 'https://staging.example/api/health',
    };
    expect(isAccessWall(probe)).toBe(false);
    expect(() => assertAccessWall(probe, 'health-anonymous')).toThrow(
      /expected Cloudflare Access denial/,
    );
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
  test('isAccessWall agrees with assertNoAccessWall on the shared fixtures', () => {
    // Cross-predicate pin from main #188: the same fixtures must read as the
    // wall under both directions of the check, using the hardened
    // header-names-only predicate (generic edge markers alone never count).
    const appShell200 = {
      status: 200,
      headers: { 'content-type': 'text/html' },
      bodyText: '<html><h1>sentinel-app-shell-heading</h1></html>',
      url: 'https://staging.example/',
    };
    for (const probe of [wall302, wall403]) {
      expect(isAccessWall(probe)).toBe(true);
      expect(() => assertNoAccessWall(probe, 'agree')).toThrow(/Access wall/);
    }
    expect(isAccessWall(appShell200)).toBe(false);
    expect(() => assertNoAccessWall(appShell200, 'agree')).not.toThrow();
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
        res.end('<title>Access | Cloudflare Access</title>');
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


test.describe('anonymousApiOptions (isolated, CORS headers only, no-follow)', () => {
  test('sends only the CORS request headers and refuses credential headers', async () => {
    const listen = (server: Server): Promise<number> =>
      new Promise((resolve, reject) => {
        server.on('error', reject);
        server.listen(0, '127.0.0.1', () => resolve((server.address() as AddressInfo).port));
      });
    const close = (server: Server): Promise<void> =>
      new Promise((resolve) => server.close(() => resolve()));
    const hits: Array<{ method?: string; headers: Record<string, string | string[] | undefined> }> =
      [];
    const staging = createServer((req: IncomingMessage, res) => {
      hits.push({ method: req.method, headers: { ...req.headers } });
      res.writeHead(403, { 'content-type': 'text/html' });
      res.end('<title>403 Forbidden</title><center>cloudflare</center>');
    });
    const port = await listen(staging);
    const origin = `http://127.0.0.1:${port}`;
    const cors = {
      Origin: origin,
      'Access-Control-Request-Method': 'GET',
      'Access-Control-Request-Headers': 'content-type',
    };

    try {
      await withEnvAsync(
        { ...dummyAccessPair, QA_AUTH_TOKEN: 'dummy-qa-token-anon-probe' },
        async () => {
          const probe = await anonymousApiOptions(`${origin}/healthz`, cors, 'preflight');
          expect(probe.status).toBe(403);
          expect(probe.bodyText).toContain('cloudflare');

          for (const name of ['CF-Access-Client-Id', 'Authorization', 'Cookie']) {
            await expect(
              anonymousApiOptions(`${origin}/healthz`, { ...cors, [name]: 'x' }, 'preflight'),
              `${name} must be refused`,
            ).rejects.toThrow(/accepts only CORS request headers/);
          }
        },
      );

      expect(hits).toHaveLength(1);
      const [hit] = hits;
      if (!hit) throw new Error('anonymous OPTIONS never reached the fixture');
      expect(hit.method).toBe('OPTIONS');
      const headers = hit.headers;
      expect(headers.origin).toBe(origin);
      expect(headers['access-control-request-method']).toBe('GET');
      expect(headers['cf-access-client-id']).toBeUndefined();
      expect(headers['cf-access-client-secret']).toBeUndefined();
      expect(headers.authorization).toBeUndefined();
      expect(headers.cookie).toBeUndefined();
      const dumped = JSON.stringify(hits);
      expect(dumped).not.toContain('dummy-client-id');
      expect(dumped).not.toContain('dummy-qa-token-anon-probe');
    } finally {
      await close(staging).catch(() => {});
    }
  });
});

test.describe('anonymousGet (no credentials, no follow)', () => {
  // The anonymous probe must send NO header material even when the QA Access
  // pair is present in the environment, and must return a 3xx as-is (never
  // follow it) so the caller can classify the wall response itself.
  test('sends no Access/Authorization/Cookie headers and returns the 302 unfollowed', async ({
    request,
  }) => {
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
    const foreignOrigin = `http://127.0.0.1:${await listen(foreign)}`;

    const stagingHits: Array<Record<string, string | string[] | undefined>> = [];
    const staging = createServer((req: IncomingMessage, res) => {
      stagingHits.push({ ...req.headers });
      if (req.url === '/redir') {
        res.writeHead(302, { location: `${foreignOrigin}/landed` });
        res.end();
        return;
      }
      res.writeHead(200, { 'content-type': 'text/html' });
      res.end('<html>shell</html>');
    });
    const stagingOrigin = `http://127.0.0.1:${await listen(staging)}`;

    try {
      await withEnvAsync(dummyAccessPair, async () => {
        const res = await anonymousGet(request, `${stagingOrigin}/redir`);
        expect(res.status(), 'anonymousGet must return the 3xx, not follow or throw').toBe(302);
        const ok = await anonymousGet(request, `${stagingOrigin}/`);
        expect(ok.status()).toBe(200);
      });
      expect(foreignHits, 'anonymous probe followed a redirect to a foreign origin').toHaveLength(0);
      expect(stagingHits).toHaveLength(2);
      for (const headers of stagingHits) {
        expect(headers['cf-access-client-id']).toBeUndefined();
        expect(headers['cf-access-client-secret']).toBeUndefined();
        expect(headers.authorization).toBeUndefined();
        expect(headers.cookie).toBeUndefined();
      }
      const dumped = JSON.stringify(stagingHits);
      expect(dumped).not.toContain(dummyAccessPair.CF_ACCESS_CLIENT_ID);
      expect(dumped).not.toContain(dummyAccessPair.CF_ACCESS_CLIENT_SECRET);
    } finally {
      await close(staging).catch(() => {});
      await close(foreign).catch(() => {});
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
          text: async () => '<title>Access | Cloudflare Access</title>',
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

test.describe('isAuthStartRequest (QA bearer scope gate, TWO-434)', () => {
  const staging = 'https://staging.example';

  test('matches POST to each auth-start route', () => {
    for (const path of [
      '/v1/auth/passkey/register/start',
      '/v1/auth/passkey/login/start',
      '/v1/auth/magic/start',
    ]) {
      expect(isAuthStartRequest('POST', `${staging}${path}`), path).toBe(true);
      expect(isAuthStartRequest('post', `${staging}${path}?x=1`), path).toBe(true);
    }
  });

  test('withholds from other methods, finishes, bearer routes and lookalikes', () => {
    expect(isAuthStartRequest('GET', `${staging}/v1/auth/magic/start`)).toBe(false);
    expect(isAuthStartRequest('OPTIONS', `${staging}/v1/auth/magic/start`)).toBe(false);
    expect(isAuthStartRequest('POST', `${staging}/v1/auth/magic/finish`)).toBe(false);
    expect(isAuthStartRequest('POST', `${staging}/v1/session/reauthenticate/start`)).toBe(false);
    expect(isAuthStartRequest('POST', `${staging}/v1/playlists`)).toBe(false);
    expect(isAuthStartRequest('POST', `${staging}/v1/auth/magic/start/`)).toBe(false);
    expect(isAuthStartRequest('POST', `${staging}/x/v1/auth/magic/start`)).toBe(false);
    expect(isAuthStartRequest('POST', 'not a url')).toBe(false);
  });
});

test.describe('applyAccountToPage QA bearer scope (browser)', () => {
  // Real Chromium check: an anonymous page sends the QA bearer on auth-start
  // POSTs only, never on navigation or other API routes; a session page
  // keeps its own bearer everywhere and never sends the QA token.
  test('anonymous page carries the QA bearer on auth-start POSTs only', async () => {
    let browser: Awaited<ReturnType<typeof chromium.launch>> | undefined;
    try {
      browser = await chromium.launch();
    } catch (error) {
      test.skip(true, `Chromium unavailable in this container (${(error as Error).message.split('\n')[0]}); runs in CI with --with-deps.`);
      return;
    }
    const markerQa = 'dummy-qa-token-start-scope';
    const markerSession = 'dummy-session-start-scope';
    const hits: Array<{ method: string; url: string; authorization: string | undefined }> = [];
    const staging = createServer((req: IncomingMessage, res) => {
      hits.push({ method: req.method ?? '', url: req.url ?? '', authorization: req.headers.authorization });
      res.writeHead(200, { 'content-type': 'text/html' });
      res.end('<p>staging</p>');
    });
    const port = await new Promise<number>((resolve, reject) => {
      staging.on('error', reject);
      staging.listen(0, '127.0.0.1', () => resolve((staging.address() as AddressInfo).port));
    });
    const stagingOrigin = `http://127.0.0.1:${port}`;
    const drive = async (account: Parameters<typeof applyAccountToPage>[1]): Promise<void> => {
      const context = await browser!.newContext();
      try {
        const page = await context.newPage();
        await applyAccountToPage(page, account, stagingOrigin);
        await page.goto(`${stagingOrigin}/signup`, { waitUntil: 'domcontentloaded' });
        await page.evaluate(async () => {
          for (const path of ['/v1/auth/passkey/register/start', '/v1/auth/magic/start', '/v1/playlists']) {
            await fetch(path, { method: 'POST', body: '{}' });
          }
          await fetch('/v1/auth/magic/start');
        });
      } finally {
        await context.close();
      }
    };
    try {
      await withEnvAsync(
        {
          CF_ACCESS_CLIENT_ID: 'dummy-client-id-start-scope',
          CF_ACCESS_CLIENT_SECRET: 'dummy-client-secret-start-scope',
          QA_AUTH_TOKEN: markerQa,
        },
        async () => {
          await drive(null);
          const anonymous = hits.splice(0);
          const auth = (method: string, url: string) =>
            anonymous.find((hit) => hit.method === method && hit.url === url)?.authorization;
          expect(auth('POST', '/v1/auth/passkey/register/start')).toBe(`Bearer ${markerQa}`);
          expect(auth('POST', '/v1/auth/magic/start')).toBe(`Bearer ${markerQa}`);
          expect(auth('POST', '/v1/playlists')).toBeUndefined();
          expect(auth('GET', '/v1/auth/magic/start')).toBeUndefined();
          expect(auth('GET', '/signup')).toBeUndefined();

          await drive({ email: 'e2e@example.test', session: markerSession, viaApi: true });
          const withSession = hits.splice(0);
          expect(withSession.length).toBeGreaterThan(0);
          for (const hit of withSession) {
            expect(hit.authorization, `${hit.method} ${hit.url}`).toBe(`Bearer ${markerSession}`);
          }
          expect(JSON.stringify(withSession)).not.toContain(markerQa);
        },
      );
    } finally {
      await new Promise<void>((resolve) => staging.close(() => resolve()));
      await browser.close().catch(() => {});
    }
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

test.describe('apiGet/apiPost/apiOptions redirect scope (no-follow)', () => {
  // API repro for the redirect leak: Playwright's APIRequestContext follows
  // redirects by default and forwards custom headers to the target. apiGet/
  // apiPost/apiOptions fetch with maxRedirects: 0 so a staging 302 to another
  // origin is returned as-is (foreign origin receives zero requests) and the
  // 3xx fails by name instead of a bare status mismatch.
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

      // OPTIONS 302 (the preflight path) behaves the same: apiOptions is the
      // only no-follow OPTIONS wrapper, so it gets its own executed leg —
      // removing its maxRedirects: 0 must fail here, not ride on apiGet's.
      await expect(
        apiOptions(request, `${stagingOrigin}/redir`, credentialed, 'probe'),
        'apiOptions followed a cross-origin 302 with credentials instead of failing',
      ).rejects.toThrow(/unexpected redirect.*HTTP 302/);
      expect(foreignHits, 'foreign origin received a credentialed follow-up to OPTIONS 302').toHaveLength(
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
            async () => fakeResponse(403, { 'cf-mitigated': 'challenge' }, '<title>Access</title><p>Cloudflare Access</p>'),
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
            async () => fakeResponse(403, { 'cf-mitigated': 'challenge' }, '<title>Access</title><p>Cloudflare Access</p>'),
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

test.describe('signup path evidence label', () => {
  const base = 'https://staging.example';
  const seamEnv = {
    STAGING_BASE_URL: base,
    QA_AUTH_TOKEN: 'dummy-qa-token',
    ...dummyAccessPair,
  };
  const permitted = ['public-ui-signup', 'api-minted-account'];

  test('seam-minted account is labelled api-minted-account, leaking no account data', async () => {
    await withEnvAsync(seamEnv, async () => {
      const minted = await tryMintTestAccount(
        {
          post: async () => ({
            status: () => 200,
            headers: () => ({}),
            text: async () =>
              '{"email":"sentinel-email@example.test","session":"sentinel-session-value"}',
          }),
        } as unknown as APIRequestContext,
        base,
      );
      const target = { annotations: [] as Array<{ type: string; description?: string }> };
      const label = annotateSignupPath(target, minted);
      expect(label).toBe('api-minted-account');
      expect(target.annotations).toEqual([{ type: 'signup-path', description: 'api-minted-account' }]);
      const dumped = JSON.stringify(target.annotations);
      expect(dumped).not.toContain('sentinel-email@example.test');
      expect(dumped).not.toContain('sentinel-session-value');
      expect(dumped).not.toContain('dummy-qa-token');
    });
  });

  test('unconfigured seam (UI signup fallback) is labelled public-ui-signup', async () => {
    await withEnvAsync({ QA_AUTH_TOKEN: undefined }, async () => {
      const minted = await tryMintTestAccount(
        {
          post: async () => {
            throw new Error('must not be called without a token');
          },
        } as unknown as APIRequestContext,
        base,
      );
      expect(minted).toBeNull();
      const target = { annotations: [] as Array<{ type: string; description?: string }> };
      expect(annotateSignupPath(target, minted)).toBe('public-ui-signup');
      expect(target.annotations).toEqual([{ type: 'signup-path', description: 'public-ui-signup' }]);
    });
  });

  test('label is always one of the two bounded values and carries no fixture data', () => {
    const fixtures = [
      null,
      { email: 'sentinel-ui@example.test', viaApi: false },
      { email: 'sentinel-api@example.test', session: 'sentinel-s2', viaApi: true },
      { email: 'sentinel-api@example.test', viaApi: true },
    ] as const;
    for (const fixture of fixtures) {
      const label = signupPathLabel(fixture);
      expect(permitted).toContain(label);
      expect(label).not.toContain('sentinel');
      expect(label).not.toContain('@');
    }
    expect(signupPathLabel({ email: 'sentinel-ui@example.test', viaApi: false })).toBe(
      'public-ui-signup',
    );
    expect(signupPathLabel({ email: 'sentinel-api@example.test', viaApi: true })).toBe(
      'api-minted-account',
    );
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

test.describe('isMbid', () => {
  test('accepts canonical lowercase and uppercase hex', () => {
    expect(isMbid('f5c2d4b1-3a7e-4d1c-9e2f-0a1b2c3d4e5f')).toBe(true);
    expect(isMbid('F5C2D4B1-3A7E-4D1C-9E2F-0A1B2C3D4E5F')).toBe(true);
  });

  test('rejects non-MBID shapes', () => {
    for (const bad of [
      '',
      'not-an-mbid',
      'f5c2d4b1-3a7e-4d1c-9e2f',
      'f5c2d4b1-3a7e-4d1c-9e2f-0a1b2c3d4e5g',
      'f5c2d4b13a7e4d1c9e2f0a1b2c3d4e5f',
      42,
      null,
    ]) {
      expect(isMbid(bad)).toBe(false);
    }
  });
});

test.describe('extractLibraryItems', () => {
  test('reads a bare array', () => {
    expect(extractLibraryItems('[{"id":"a"},{"id":"b","title":"T"}]')).toEqual([
      { id: 'a' },
      { id: 'b', title: 'T' },
    ]);
  });

  test('reads the { items: [...] } shape', () => {
    expect(extractLibraryItems('{"items":[{"id":"a"}]}')).toEqual([{ id: 'a' }]);
  });

  test('drops rows without a string id', () => {
    expect(extractLibraryItems('[{"id":"a"},{},{"id":42}]')).toEqual([{ id: 'a' }]);
  });

  test('throws by name on non-JSON and reshaped payloads', () => {
    expect(() => extractLibraryItems('not json')).toThrow(/\[library\]/);
    expect(() => extractLibraryItems('{"tracks":[]}')).toThrow(/tracks/);
  });
});

test.describe('extractRecordingMbids', () => {
  const MBID = 'f5c2d4b1-3a7e-4d1c-9e2f-0a1b2c3d4e5f';

  test('reads the canonical field', () => {
    expect(extractRecordingMbids({ id: 't', recording_mbids: [MBID] })).toEqual([MBID]);
  });

  test('reads the camelCase alias', () => {
    expect(extractRecordingMbids({ id: 't', recordingMbids: [MBID] })).toEqual([MBID]);
  });

  test('names the actual keys when the field is absent', () => {
    expect(() => extractRecordingMbids({ id: 't', title: 'x' })).toThrow(/title/);
  });

  test('an empty list fails naming the release state', () => {
    expect(() => extractRecordingMbids({ id: 't', recording_mbids: [] })).toThrow(
      /no release_mbid/,
    );
    expect(() =>
      extractRecordingMbids({ id: 't', release_mbid: 'r', recording_mbids: [] }),
    ).toThrow(/release_mbid r/);
  });

  test('a non-array and malformed entries fail by name', () => {
    expect(() => extractRecordingMbids({ id: 't', recording_mbids: 'x' })).toThrow(/array/);
    expect(() => extractRecordingMbids({ id: 't', recording_mbids: ['zzz'] })).toThrow(
      /recording_mbids\[0\]/,
    );
    expect(() => extractRecordingMbids(null)).toThrow(/object/);
  });
});

test.describe('plan-limit refusal verdict (offline pins)', () => {
  const clean402 = {
    status: 402,
    headers: { 'x-plan-limit': '100', 'x-plan-reset': '3600' },
    bodyText: JSON.stringify({
      code: 'plan_limit_exceeded',
      message: 'plan limit reached',
      retryable: false,
      upgrade_url: '/pricing',
    }),
  };
  const secrets = [
    { name: 'account email', value: 'capped+sentinel@example.test' },
    { name: 'account session', value: 'sentinel-session-capped' },
  ];

  test('a canonical 402 refusal passes', () => {
    expect(() =>
      assertPlanLimitRefusal({ ...clean402, secrets, context: 'plan-limit-stream' }),
    ).not.toThrow();
  });

  test('a 429 with retry-after + limit header passes', () => {
    expect(() =>
      assertPlanLimitRefusal({
        status: 429,
        headers: { 'ratelimit-limit': '50', 'retry-after': '45' },
        bodyText: JSON.stringify({
          code: 'rate_limited',
          message: 'too many requests',
          retryable: true,
          upgrade_url: '/pricing',
        }),
        secrets,
        context: 'plan-limit-ingest',
      }),
    ).not.toThrow();
  });

  test('a non-limit status fails naming expected vs actual', () => {
    expect(() =>
      assertPlanLimitRefusal({ ...clean402, status: 206, secrets, context: 'plan-limit-stream' }),
    ).toThrow(/expected a plan-limit status \(402\/429\), got HTTP 206/);
  });

  test('missing headers fail naming the received names', () => {
    expect(() =>
      assertPlanLimitRefusal({
        ...clean402,
        headers: { 'content-type': 'application/json' },
        secrets,
        context: 'plan-limit-stream',
      }),
    ).toThrow(/missing .*limit header.*got headers: content-type/);
    expect(() =>
      assertPlanLimitRefusal({
        ...clean402,
        headers: { 'x-plan-limit': '100' },
        secrets,
        context: 'plan-limit-stream',
      }),
    ).toThrow(/reset\/retry header.*got headers: x-plan-limit/);
  });

  test('extractLimitCode reads the envelope and legacy shapes', () => {
    expect(
      extractLimitCode(JSON.stringify({ code: 'plan_limit_exceeded' }), 'plan-limit-stream'),
    ).toBe('plan_limit_exceeded');
    expect(extractLimitCode('{"error":"rate_limited"}', 'plan-limit-stream')).toBe('rate_limited');
    expect(
      extractLimitCode('{"error":{"code":"plan_limit_exceeded"}}', 'plan-limit-stream'),
    ).toBe('plan_limit_exceeded');
  });

  test('extractLimitCode names the keys on drift', () => {
    expect(() =>
      extractLimitCode(JSON.stringify({ status: 'limited' }), 'plan-limit-stream'),
    ).toThrow(/\[plan-limit-stream\][\s\S]*status/);
    expect(() => extractLimitCode('not json', 'plan-limit-stream')).toThrow(/not JSON/);
  });

  test('a status-inconsistent retryable flag fails', () => {
    expect(() =>
      assertPlanLimitRefusal({
        ...clean402,
        bodyText: JSON.stringify({
          code: 'plan_limit_exceeded',
          message: 'plan limit reached',
          retryable: true,
          upgrade_url: '/pricing',
        }),
        secrets,
        context: 'plan-limit-stream',
      }),
    ).toThrow(/retryable flag disagrees/);
  });

  test('extractUpgradeCta accepts a key or a /pricing value', () => {
    expect(extractUpgradeCta({ code: 'x', upgrade_url: '/pricing' }, 'c')).toBe("key 'upgrade_url'");
    expect(extractUpgradeCta({ code: 'x', note: 'see /pricing to continue' }, 'c')).toMatch(
      /value/,
    );
  });

  test('prose without the /pricing route is not a CTA pointer', () => {
    expect(() => extractUpgradeCta({ code: 'x', note: 'Upgrade now' }, 'c')).toThrow(
      /no upgrade CTA pointer/,
    );
  });

  test('a matching key with a non-route value is not a CTA pointer', () => {
    expect(() => extractUpgradeCta({ code: 'x', upgrade_available: false }, 'c')).toThrow(
      /no upgrade CTA pointer/,
    );
    expect(() => extractUpgradeCta({ code: 'x', upgrade_url: '' }, 'c')).toThrow(
      /no upgrade CTA pointer/,
    );
    expect(() =>
      extractUpgradeCta({ code: 'x', upgrade_url: 'Upgrade now, no route' }, 'c'),
    ).toThrow(/no upgrade CTA pointer/);
  });

  test('a nested /pricing route behind a matching key still counts', () => {
    expect(extractUpgradeCta({ code: 'x', cta: { href: '/pricing' } }, 'c')).toMatch(/cta/);
  });

  test('a body without the CTA pointer fails naming its keys', () => {
    expect(() =>
      extractUpgradeCta({ code: 'plan_limit_exceeded', message: 'nope' }, 'plan-limit-stream'),
    ).toThrow(/\[plan-limit-stream\][\s\S]*code, message/);
    expect(() =>
      assertPlanLimitRefusal({
        ...clean402,
        bodyText: JSON.stringify({ code: 'plan_limit_exceeded', retryable: false }),
        secrets,
        context: 'plan-limit-stream',
      }),
    ).toThrow(/no upgrade CTA pointer/);
  });

  test('secretLeak names the secret, never its value', () => {
    const body = JSON.stringify({ code: 'x', detail: 'for capped+sentinel@example.test' });
    const leak = secretLeak(body, secrets, 'plan-limit-stream');
    expect(leak).toMatch(/account email/);
    expect(leak).not.toContain('capped+sentinel@example.test');
    const keyed = secretLeak(JSON.stringify({ token: 'abc' }), [], 'plan-limit-stream');
    expect(keyed).toMatch(/credential-shaped key 'token'/);
    expect(secretLeak(clean402.bodyText, secrets, 'plan-limit-stream')).toBeNull();
  });

  test('a leaking refusal body fails the verdict', () => {
    expect(() =>
      assertPlanLimitRefusal({
        ...clean402,
        bodyText: JSON.stringify({
          code: 'plan_limit_exceeded',
          retryable: false,
          upgrade_url: '/pricing',
          session: 'sentinel-session-capped',
        }),
        secrets,
        context: 'plan-limit-stream',
      }),
    ).toThrow(/leaks the account session/);
  });

  test('assertPlanLimitHeaders tolerates header-name variants', () => {
    expect(() =>
      assertPlanLimitHeaders(
        { 'RateLimit-Limit': '10', 'RateLimit-Reset': '60' },
        'plan-limit-stream',
      ),
    ).not.toThrow();
    expect(() =>
      assertPlanLimitHeaders({ 'X-Plan-Limit': '', 'Retry-After': '5' }, 'plan-limit-stream'),
    ).toThrow(/present but empty/);
  });

  test('a 429 without retry-after fails even with another reset header', () => {
    expect(() =>
      assertPlanLimitHeaders(
        { 'x-plan-limit': '50', 'x-plan-reset': '45' },
        'plan-limit-ingest',
        429,
      ),
    ).toThrow(/retry-after/);
    expect(() =>
      assertPlanLimitHeaders({ 'x-plan-limit': '50', 'retry-after': '' }, 'plan-limit-ingest', 429),
    ).toThrow(/retry-after|present but empty/);
    expect(() =>
      assertPlanLimitRefusal({
        status: 429,
        headers: { 'ratelimit-limit': '50', 'ratelimit-reset': '45' },
        bodyText: JSON.stringify({
          code: 'rate_limited',
          message: 'too many requests',
          retryable: true,
          upgrade_url: '/pricing',
        }),
        secrets,
        context: 'plan-limit-ingest',
      }),
    ).toThrow(/retry-after/);
  });

  test('a status-inconsistent limit code fails', () => {
    expect(() =>
      assertPlanLimitRefusal({
        ...clean402,
        bodyText: JSON.stringify({
          code: 'rate_limited',
          message: 'too many requests',
          retryable: false,
          upgrade_url: '/pricing',
        }),
        secrets,
        context: 'plan-limit-stream',
      }),
    ).toThrow(/code 'rate_limited' disagrees with the status/);
    expect(() =>
      assertPlanLimitRefusal({
        status: 429,
        headers: { 'ratelimit-limit': '50', 'retry-after': '45' },
        bodyText: JSON.stringify({
          code: 'plan_limit_exceeded',
          message: 'plan limit reached',
          retryable: true,
          upgrade_url: '/pricing',
        }),
        secrets,
        context: 'plan-limit-ingest',
      }),
    ).toThrow(/code 'plan_limit_exceeded' disagrees with the status/);
  });

  test('a missing retryable flag fails', () => {
    expect(() =>
      assertPlanLimitRefusal({
        ...clean402,
        bodyText: JSON.stringify({
          code: 'plan_limit_exceeded',
          message: 'plan limit reached',
          upgrade_url: '/pricing',
        }),
        secrets,
        context: 'plan-limit-stream',
      }),
    ).toThrow(/retryable flag disagrees/);
  });
});

// Signup outcome diagnostics (offline, static pages): when neither the code
// step nor the inline error appears, the failure must name the page state
// and the registerStart status — never a field value.
test.describe('signupInlineErrorMessage names the registerStart status', () => {
  test('an observed status is named', () => {
    expect(signupInlineErrorMessage({ status: 500 })).toContain('registerStart HTTP 500');
  });

  test('a request with no response is named as not observed', () => {
    expect(signupInlineErrorMessage({ status: null })).toContain('registerStart HTTP not observed');
  });

  // Static pin: both public-signup helpers must assert the inline error
  // through the formatter, so reverting either one to a constant message
  // (losing the status on a red staging leg) fails offline.
  for (const name of ['submitPublicUiSignup', 'submitPublicUiSignupToByopWizard']) {
    test(`${name} asserts the inline error with the registerStart status`, () => {
      const source = readFileSync(new URL('../helpers/accounts.ts', import.meta.url), 'utf8');
      const marker = `export async function ${name}(`;
      const start = source.indexOf(marker);
      expect(start, `${name} is missing`).toBeGreaterThan(-1);
      const rest = source.slice(start + marker.length);
      const next = rest.search(/\nexport /);
      const body = next === -1 ? rest : rest.slice(0, next);
      expect(body).toContain('signupInlineErrorMessage(registerStatus)');
      expect(body).not.toContain("'public UI signup showed an inline error'");
    });
  }
});

test.describe('awaitSignupOutcome names the signup page state', () => {
  const page_ = (body: string) =>
    `<!doctype html><html><body><input data-testid="signup-email" value="SENTINEL@example.test" />${body}</body></html>`;
  const failure = async (
    page: import('@playwright/test').Page,
    body: string,
    status: number | null,
  ): Promise<string> => {
    await page.setContent(page_(body));
    try {
      await awaitSignupOutcome(
        page,
        page.locator('input[name="magic_code"]'),
        page.getByTestId('signup-error'),
        { status },
        200,
      );
    } catch (error) {
      return (error as Error).message;
    }
    return '';
  };

  test('code step or inline error settles without throwing', async ({ page }) => {
    expect(await failure(page, '<input name="magic_code" />', 200)).toBe('');
    expect(await failure(page, '<p data-testid="signup-error">x</p>', 500)).toBe('');
  });

  test('plan-limit notice names a paywall/limit answer and its status', async ({ page }) => {
    const message = await failure(page, '<dialog open data-testid="plan-limit-notice">limit</dialog>', 429);
    expect(message).toContain('plan-limit notice open');
    expect(message).toContain('registerStart HTTP 429');
    expect(message).not.toContain('SENTINEL');
  });

  test('pending request names the sending state', async ({ page }) => {
    const message = await failure(page, '<p role="status">Sending your email code…</p>', null);
    expect(message).toContain('still sending');
    expect(message).toContain('registerStart HTTP not observed');
  });

  test('the registerStart observer feeds the real response status to the diagnostic', async ({
    page,
  }) => {
    // A local page whose submit POSTs registerStart and gets a 429, like a
    // rate-limited staging answer; the page then shows nothing.
    const server = createServer((req, res) => {
      if (req.method === 'POST' && req.url === '/v1/auth/passkey/register/start') {
        res.writeHead(429, { 'content-type': 'application/json' });
        res.end('{"code":"rate_limited"}');
        return;
      }
      res.writeHead(200, { 'content-type': 'text/html' });
      res.end(
        '<!doctype html><html><body><button id="go">go</button><script>' +
          "document.getElementById('go').onclick = () => " +
          "fetch('/v1/auth/passkey/register/start', { method: 'POST' });" +
          '</script></body></html>',
      );
    });
    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
    try {
      const { port } = server.address() as AddressInfo;
      await page.goto(`http://127.0.0.1:${port}/`);
      const registerStatus = recordRegisterStartStatus(page);
      await page.locator('#go').click();
      await expect.poll(() => registerStatus.status).toBe(429);
      let message = '';
      try {
        await awaitSignupOutcome(
          page,
          page.locator('input[name="magic_code"]'),
          page.getByTestId('signup-error'),
          registerStatus,
          200,
        );
      } catch (error) {
        message = (error as Error).message;
      }
      expect(message).toContain('registerStart HTTP 429');
    } finally {
      await new Promise<void>((resolve) => server.close(() => resolve()));
    }
  });

  test('unsupported passkeys and a bare email step are named', async ({ page }) => {
    expect(
      await failure(page, '<p role="status">This browser does not support passkeys.</p>', null),
    ).toContain('passkeys unsupported');
    expect(await failure(page, '', 201)).toContain('back at the email step with no status');
  });
});
