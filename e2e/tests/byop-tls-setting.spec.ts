import { test, expect } from '@playwright/test';
import { syntheticNntpEndpoint } from '../helpers/access.js';

// Malformed SYNTHETIC_NNTP_TLS spellings must be rejected, not silently
// treated as TLS-on.
//
// The paired acceptance spec (byop-transport.spec.ts) covers the literal
// 'off' spelling. This spec pins the remaining guard: any value other than
// 'on' (or unset) — including the realistic "TLS off" spellings false/0/no
// that the pre-fix harness silently treated as TLS-on — throws a names-only
// diagnostic before any page interaction. Kept in its own file so the
// paired acceptance spec stays unedited.

const DUMMY_PASS = 'dummy-byop-pass-canary';

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

const fixtureEnv = (overrides: Record<string, string | undefined> = {}) => ({
  SYNTHETIC_NNTP_HOST: 'synthetic-nntp',
  SYNTHETIC_NNTP_USER: 'e2e',
  SYNTHETIC_NNTP_PASS: DUMMY_PASS,
  SYNTHETIC_NNTP_PORT: '563',
  SYNTHETIC_NNTP_TLS: 'on',
  ...overrides,
});

test.describe('syntheticNntpEndpoint malformed TLS setting', () => {
  for (const value of ['false', '0', 'no', 'true', 'yes', 'maybe', '  ']) {
    test(`rejects SYNTHETIC_NNTP_TLS=${JSON.stringify(value)}`, () => {
      withEnv(fixtureEnv({ SYNTHETIC_NNTP_TLS: value }), () => {
        let message = '';
        try {
          syntheticNntpEndpoint();
        } catch (error) {
          message = (error as Error).message;
        }
        expect(message, `TLS setting ${JSON.stringify(value)} was silently accepted`).toMatch(
          /SYNTHETIC_NNTP_TLS/,
        );
        expect(message).not.toContain(DUMMY_PASS);
      });
    });
  }
});
