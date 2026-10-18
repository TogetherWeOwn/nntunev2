import { test, expect } from '@playwright/test';
import { API } from '../helpers/contract.js';
import { accessHeaders, assertNoAccessWall, stagingBaseUrl } from '../helpers/access.js';
import { apiGet } from '../helpers/accounts.js';
import {
  assertDeployedBuildMatchesPromotion,
  expectedPromotionSha,
  extractDeployedSha,
} from '../helpers/build-identity.js';

// Same-build assertion for exact-head staging evidence (row 7 remainder):
// the deployed build identity (`GET /api/build`, baked at build time from
// the promotion commit) must equal the promotion SHA from the
// push-triggered deploy run (`EXPECTED_BUILD_SHA` via deploy.yml
// `expected_sha` from `github.sha`) — never a local source checkout.
//
// NO file-level skip by design: the offline describe below pins the verdict
// contract on every PR (via `npm run test:unit`) with no staging, while the
// live leg skips green when STAGING_BASE_URL is unset and fails closed once
// it is configured. A mismatch or an `unknown` identity fails by name, never
// skip-green. Runs only in the full journey (`npm run e2e:staging`): the
// synthetic smoke (`npm run e2e:smoke`) runs `health.spec.ts` alone, so
// scheduled/manual monitoring can never mint same-build evidence.

test.describe('build identity assertion (offline fixtures)', () => {
  const DEPLOYED = 'a'.repeat(40);
  const PROMOTION = 'a'.repeat(40);
  const OTHER = 'b'.repeat(40);

  test('matching 40-hex SHAs pass', () => {
    expect(() =>
      assertDeployedBuildMatchesPromotion(DEPLOYED, PROMOTION, 'build-identity'),
    ).not.toThrow();
  });

  test('mismatch fails naming the build identity gap', () => {
    expect(() =>
      assertDeployedBuildMatchesPromotion(OTHER, PROMOTION, 'build-identity'),
    ).toThrow(/\[build-identity\].*build identity mismatch/);
  });

  test('mismatch names both SHAs (public commit SHAs, never secrets)', () => {
    let message = '';
    try {
      assertDeployedBuildMatchesPromotion(OTHER, PROMOTION, 'build-identity');
    } catch (error) {
      message = error instanceof Error ? error.message : String(error);
    }
    expect(message).toContain(OTHER);
    expect(message).toContain(PROMOTION);
  });

  test('unknown deployed identity fails naming unknown', () => {
    expect(() =>
      assertDeployedBuildMatchesPromotion('unknown', PROMOTION, 'build-identity'),
    ).toThrow(/\[build-identity\].*unknown/);
  });

  test('malformed deployed sha fails naming the build identity gap', () => {
    expect(() =>
      assertDeployedBuildMatchesPromotion('not-a-sha', PROMOTION, 'build-identity'),
    ).toThrow(/\[build-identity\].*build identity is malformed/);
  });

  test('malformed promotion SHA fails naming EXPECTED_BUILD_SHA', () => {
    expect(() =>
      assertDeployedBuildMatchesPromotion(DEPLOYED, 'not-a-sha', 'build-identity'),
    ).toThrow(/EXPECTED_BUILD_SHA/);
  });

  test('missing promotion SHA fails naming EXPECTED_BUILD_SHA', () => {
    const saved = process.env.EXPECTED_BUILD_SHA;
    delete process.env.EXPECTED_BUILD_SHA;
    try {
      expect(() => expectedPromotionSha('build-identity')).toThrow(/EXPECTED_BUILD_SHA/);
    } finally {
      if (saved !== undefined) process.env.EXPECTED_BUILD_SHA = saved;
    }
  });

  test('malformed promotion env fails naming EXPECTED_BUILD_SHA', () => {
    const saved = process.env.EXPECTED_BUILD_SHA;
    process.env.EXPECTED_BUILD_SHA = 'not-a-sha';
    try {
      expect(() => expectedPromotionSha('build-identity')).toThrow(/EXPECTED_BUILD_SHA/);
    } finally {
      if (saved !== undefined) process.env.EXPECTED_BUILD_SHA = saved;
      else delete process.env.EXPECTED_BUILD_SHA;
    }
  });

  test('valid promotion env is returned verbatim', () => {
    const saved = process.env.EXPECTED_BUILD_SHA;
    process.env.EXPECTED_BUILD_SHA = PROMOTION;
    try {
      expect(expectedPromotionSha('build-identity')).toBe(PROMOTION);
    } finally {
      if (saved !== undefined) process.env.EXPECTED_BUILD_SHA = saved;
      else delete process.env.EXPECTED_BUILD_SHA;
    }
  });

  test('valid build body extracts the sha', () => {
    expect(extractDeployedSha(`{"sha":"${DEPLOYED}"}`, 'build-identity')).toBe(DEPLOYED);
  });

  test('unknown build body extracts unknown (the verdict fails it, not the parser)', () => {
    expect(extractDeployedSha('{"sha":"unknown"}', 'build-identity')).toBe('unknown');
  });

  test('non-JSON build body fails naming the build identity gap', () => {
    expect(() => extractDeployedSha('not json', 'build-identity')).toThrow(
      /\[build-identity\].*build identity body is not JSON/,
    );
  });

  test('build body with extra keys fails naming the keys', () => {
    expect(() =>
      extractDeployedSha(`{"sha":"${DEPLOYED}","version":"1.0.0"}`, 'build-identity'),
    ).toThrow(/exactly \{ sha \}/);
  });

  test('build body with a missing sha fails naming the gap', () => {
    expect(() => extractDeployedSha('{}', 'build-identity')).toThrow(
      /\[build-identity\].*exactly \{ sha \}/,
    );
  });

  test('failure messages name the caller context', () => {
    expect(() =>
      assertDeployedBuildMatchesPromotion(OTHER, PROMOTION, 'build-identity'),
    ).toThrow(/\[build-identity\]/);
    expect(() => extractDeployedSha('{}', 'build-identity')).toThrow(/\[build-identity\]/);
  });
});

test('deployed build matches the promotion SHA (exact-head staging evidence)', async ({
  request,
}) => {
  const baseURL = stagingBaseUrl('skip', 'build-identity');
  if (!baseURL) {
    test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
    return;
  }
  // Promotion SHA from the push-triggered deploy run (EXPECTED_BUILD_SHA via
  // deploy.yml expected_sha from github.sha) — never a source checkout.
  // Missing or malformed values throw here: fail closed, never skip-green.
  const expected = expectedPromotionSha('build-identity');
  // apiGet fetches with maxRedirects: 0 so a staging 302 never carries the
  // Access pair to another origin (see helpers/accounts.ts).
  const res = await apiGet(request, `${baseURL}${API.build}`, accessHeaders(), 'build-identity');
  const bodyText = await res.text().catch(() => '');
  assertNoAccessWall(
    { status: res.status(), headers: res.headers(), bodyText, url: `${baseURL}${API.build}` },
    'build-identity',
  );
  expect(res.status(), `build body: ${bodyText.slice(0, 500)}`).toBe(200);
  const deployed = extractDeployedSha(bodyText, 'build-identity');
  assertDeployedBuildMatchesPromotion(deployed, expected, 'build-identity');
});
