import { test, expect, type APIRequestContext } from '@playwright/test';
import {
  accessHeaders,
  assertNoAccessWall,
  isStagingOrigin,
  readApiBody,
  stagingBaseUrl,
  syntheticNntpEndpoint,
} from '../helpers/access.js';
import { extractPlanMeta, extractUsageBytes } from '../helpers/contract.js';
import { requireApiAccount, tryMintTestAccount } from '../helpers/accounts.js';

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

  test('passes on the seam token alone', () => {
    withEnv({ QA_AUTH_TOKEN: 'dummy-qa-token' }, () => {
      expect(() => requireApiAccount(null, 'meter')).not.toThrow();
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
