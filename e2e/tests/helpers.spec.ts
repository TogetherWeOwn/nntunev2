import { test, expect } from '@playwright/test';
import {
  accessHeaders,
  assertNoAccessWall,
  stagingBaseUrl,
  syntheticNntpEndpoint,
} from '../helpers/access.js';
import { extractPlanMeta, extractUsageBytes } from '../helpers/contract.js';

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

test.describe('accessHeaders', () => {
  test('empty when the pair is unset', () => {
    withEnv({ CF_ACCESS_CLIENT_ID: undefined, CF_ACCESS_CLIENT_SECRET: undefined }, () => {
      expect(accessHeaders()).toEqual({});
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

  test('strips trailing slashes', () => {
    withEnv({ STAGING_BASE_URL: 'https://staging.example///' }, () => {
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
