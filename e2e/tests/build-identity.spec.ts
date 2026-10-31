import { test, expect } from '@playwright/test';
import { stagingBaseUrl } from '../helpers/access.js';
import { assertServingBuildMatchesPromotion } from '../helpers/build-identity-live.js';
import {
  assertApiBuildMatchesPromotion,
  assertDeployedBuildMatchesPromotion,
  expectedPromotionSha,
  extractApiBuildSha,
  extractDeployedSha,
  resolveApiIdentity,
} from '../helpers/build-identity.js';

// Same-build assertion for exact-head staging evidence (row 7 remainder):
// the deployed build identity (`GET /api/build`, baked at build time from
// the promotion commit) must equal the promotion SHA from the
// push-triggered deploy run (`EXPECTED_BUILD_SHA` via deploy.yml
// `expected_sha` from `github.sha`) — never a local source checkout.
//
// This file holds the OPENING read plus the offline verdict pins. A single
// point-in-time read cannot bind a ~30 min journey: a later main push may
// deploy staging mid-journey after this read passes. The CLOSING read
// (`e2e/tests/zz-build-identity-final.spec.ts`, `zz-` prefix sorts it last
// under the serial `workers: 1` config) re-reads `/api/build` after every
// other spec through the same shared helper below and fails with a
// "superseded by a newer deploy" message when the serving build changed —
// so a green run proves the whole journey ran against the promoted SHA.
// Deploy concurrency is intentionally unchanged (job-level groups release
// when the deploy finishes); the closing read turns the race into a red
// run instead of a false pass.
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

test.describe('API build identity assertion (offline fixtures)', () => {
  const HEAD = 'a'.repeat(40);
  const OLD = 'b'.repeat(40);

  test('healthz body with build_sha extracts it', () => {
    expect(extractApiBuildSha(`{"ok":true,"build_sha":"${HEAD}"}`, 'api')).toBe(HEAD);
  });

  test('pre-identity healthz body {ok:true} extracts null', () => {
    expect(extractApiBuildSha('{"ok":true}', 'api')).toBeNull();
  });

  test('healthz body with extra keys or without ok:true fails', () => {
    expect(() => extractApiBuildSha(`{"ok":true,"build_sha":"${HEAD}","x":1}`, 'api')).toThrow(
      /exactly \{ ok, build_sha \}/,
    );
    expect(() => extractApiBuildSha(`{"ok":false,"build_sha":"${HEAD}"}`, 'api')).toThrow(/ok:true/);
    expect(() => extractApiBuildSha('nope', 'api')).toThrow(/not JSON/);
  });

  for (const outcome of ['deployed', '']) {
    test(`staging-api "${outcome}": equal SHA is web = API = head`, () => {
      expect(assertApiBuildMatchesPromotion(HEAD, HEAD, outcome, 'api').sameBuild).toBe(true);
    });

    test(`staging-api "${outcome}": mismatch, unknown and absent fail by name`, () => {
      expect(() => assertApiBuildMatchesPromotion(OLD, HEAD, outcome, 'api')).toThrow(
        /\[api\] API build identity mismatch/,
      );
      expect(() => assertApiBuildMatchesPromotion('unknown', HEAD, outcome, 'api')).toThrow(
        /API build identity is unknown/,
      );
      expect(() => assertApiBuildMatchesPromotion(null, HEAD, outcome, 'api')).toThrow(
        /API build identity is absent/,
      );
      expect(() => assertApiBuildMatchesPromotion('nope', HEAD, outcome, 'api')).toThrow(
        /API build identity is malformed/,
      );
    });
  }

  test('unwired outcome names API_DEPLOY_OUTCOME on mismatch', () => {
    expect(() => assertApiBuildMatchesPromotion(OLD, HEAD, '', 'api')).toThrow(/API_DEPLOY_OUTCOME/);
  });

  for (const outcome of ['skipped', 'unconfigured']) {
    test(`staging-api "${outcome}": differing API SHA is stated, not claimed`, () => {
      const verdict = assertApiBuildMatchesPromotion(OLD, HEAD, outcome, 'api');
      expect(verdict.sameBuild).toBe(false);
      expect(verdict.note).toContain(`staging-api ${outcome}`);
      expect(verdict.note).toContain(OLD);
      expect(verdict.note).toContain(HEAD);
      expect(verdict.note).toContain('NOT claimed');
    });

    test(`staging-api "${outcome}": an equal API SHA is still not same-run evidence`, () => {
      const verdict = assertApiBuildMatchesPromotion(HEAD, HEAD, outcome, 'api');
      expect(verdict.sameBuild).toBe(false);
      expect(verdict.note).toContain('NOT claimed');
    });
  }

  for (const outcome of ['skipped', 'unconfigured']) {
    test(`staging-api "${outcome}": a non-200 /healthz records no API identity`, () => {
      const verdict = resolveApiIdentity(404, 'Not Found', HEAD, outcome, 'api');
      expect(verdict.apiSha).toBeNull();
      expect(verdict.sameBuild).toBe(false);
      expect(verdict.note).toContain('HTTP 404');
      expect(verdict.note).toContain('NOT claimed');
    });
  }

  for (const outcome of ['deployed', '']) {
    test(`staging-api "${outcome}": a non-200 /healthz fails`, () => {
      expect(() => resolveApiIdentity(404, 'Not Found', HEAD, outcome, 'api')).toThrow(
        /\[api\] API build identity request failed.*HTTP 404/,
      );
    });
  }

  test('a 200 /healthz flows through the strict verdict', () => {
    const body = `{"ok":true,"build_sha":"${HEAD}"}`;
    expect(resolveApiIdentity(200, body, HEAD, 'deployed', 'api')).toEqual({
      apiSha: HEAD,
      sameBuild: true,
      note: '',
    });
    expect(() =>
      resolveApiIdentity(200, `{"ok":true,"build_sha":"${OLD}"}`, HEAD, 'deployed', 'api'),
    ).toThrow(/API build identity mismatch/);
  });

  test('failed or cancelled staging-api never passes', () => {
    for (const outcome of ['failed', 'cancelled', 'surprise']) {
      expect(() => assertApiBuildMatchesPromotion(HEAD, HEAD, outcome, 'api')).toThrow(
        /staging-api outcome/,
      );
    }
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
  // Opening read: proves the deploy delivered the promoted build (web and
  // API, logged as web=… api=… head=…) before the journey. The closing spec re-reads at the end through the same helper —
  // a mid-journey deploy reds the run instead of passing green.
  await assertServingBuildMatchesPromotion(request, baseURL, 'build-identity');
});
