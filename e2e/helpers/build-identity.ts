/**
 * Deployed-build identity assertion for exact-head staging evidence.
 *
 * Staging builds bake the promotion commit SHA into `GET /api/build`
 * (`apps/web/src/lib/build.ts` via `vite.config.ts`, sourced from
 * `BUILD_SHA` / `GITHUB_SHA` at build time). The post-deploy E2E proves the
 * serving build is the promoted one by comparing that baked identity with
 * the promotion SHA from the push-triggered deploy run — never with a local
 * source checkout (`git rev-parse` would prove what is checked out, not what
 * is serving).
 *
 * Pure except for `expectedPromotionSha`, which reads `process.env`.
 * Nothing here skips: callers skip pre-deploy on `STAGING_BASE_URL`, and
 * once staging is configured every gap below throws by name.
 */

const SHA40 = /^[0-9a-f]{40}$/;

/**
 * Read the promotion SHA the deploy run promoted.
 *
 * The value arrives as `EXPECTED_BUILD_SHA`, set by `deploy.yml` from
 * `${{ github.sha }}` and threaded through `e2e-staging.yml`
 * (`inputs.expected_sha`). It must be a 40-char lowercase hex SHA.
 * Missing, empty or malformed values throw naming the variable — a
 * configured run without a promotion SHA can never pass silently.
 */
export function expectedPromotionSha(context: string): string {
  const raw = (process.env.EXPECTED_BUILD_SHA || '').trim();
  if (!raw) {
    throw new Error(
      `[${context}] build identity has no promotion SHA: EXPECTED_BUILD_SHA is not set. ` +
        'The post-deploy E2E must receive the promotion SHA from the push-triggered ' +
        'deploy run (deploy.yml expected_sha from github.sha) — refusing to pass ' +
        'without it. See e2e/README.md.',
    );
  }
  if (!SHA40.test(raw)) {
    throw new Error(
      `[${context}] build identity promotion SHA is malformed: EXPECTED_BUILD_SHA must be ` +
        'a 40-char lowercase hex commit SHA from the push-triggered deploy run ' +
        '(deploy.yml expected_sha from github.sha). See e2e/README.md.',
    );
  }
  return raw;
}

/**
 * Pull the deployed build SHA out of a `GET /api/build` body.
 *
 * The surface returns exactly `{ sha }`, where `sha` is a 40-char lowercase
 * hex commit SHA or the explicit string `"unknown"` when the build carried
 * no identity. Extra keys, a missing key, non-JSON or a null/empty sha throw
 * naming the gap — drift fails by name instead of passing silently. The
 * returned value may be `"unknown"`: callers pass it to
 * `assertDeployedBuildMatchesPromotion`, which fails it by name.
 */
export function extractDeployedSha(bodyText: string, context: string): string {
  let parsed: unknown;
  try {
    parsed = JSON.parse(bodyText) as unknown;
  } catch {
    throw new Error(
      `[${context}] build identity body is not JSON (${bodyText.length} chars received): ` +
        'expected exactly { sha } from GET /api/build. See e2e/README.md.',
    );
  }
  if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) {
    throw new Error(
      `[${context}] build identity body is not a JSON object: expected exactly ` +
        '{ sha } from GET /api/build. See e2e/README.md.',
    );
  }
  const keys = Object.keys(parsed as Record<string, unknown>).sort();
  if (keys.length !== 1 || keys[0] !== 'sha') {
    throw new Error(
      `[${context}] build identity body must carry exactly { sha } from GET /api/build, ` +
        `got keys: ${keys.join(', ') || '(empty)'}. ` +
        'If the web card reshaped the surface, update the surface + this spec together.',
    );
  }
  const sha = (parsed as Record<string, unknown>).sha;
  if (typeof sha !== 'string' || sha.length === 0) {
    throw new Error(
      `[${context}] build identity sha is missing: expected a 40-hex SHA or the explicit ` +
        'string "unknown" under { sha } from GET /api/build. See e2e/README.md.',
    );
  }
  return sha;
}

/**
 * Fail unless the deployed build SHA equals the promotion SHA.
 *
 * Passes only when both are 40-char lowercase hex and strictly equal.
 * An `"unknown"` deployed identity, a malformed deployed value, a malformed
 * expected value, or any mismatch throws naming the gap with both SHAs
 * (commit SHAs are public, never secrets) — never skip-green.
 */
export function assertDeployedBuildMatchesPromotion(
  deployedSha: string,
  expectedSha: string,
  context: string,
): void {
  if (!SHA40.test(expectedSha)) {
    throw new Error(
      `[${context}] build identity promotion SHA is malformed: expected a 40-char ` +
        'lowercase hex commit SHA from the push-triggered deploy run ' +
        '(EXPECTED_BUILD_SHA via deploy.yml expected_sha). See e2e/README.md.',
    );
  }
  if (deployedSha === 'unknown') {
    throw new Error(
      `[${context}] build identity is unknown: GET /api/build reported the explicit ` +
        `"unknown" identity instead of the promotion SHA ${expectedSha}. ` +
        'The staging build carried no baked SHA — refusing to claim same-build evidence. ' +
        'See e2e/README.md.',
    );
  }
  if (!SHA40.test(deployedSha)) {
    throw new Error(
      `[${context}] build identity is malformed: GET /api/build reported a sha that is ` +
        'neither 40-hex nor the explicit "unknown" string. ' +
        `Promotion SHA ${expectedSha}. See e2e/README.md.`,
    );
  }
  if (deployedSha !== expectedSha) {
    throw new Error(
      `[${context}] build identity mismatch: deployed build ${deployedSha} does not match ` +
        `the promotion SHA ${expectedSha} from the push-triggered deploy run. ` +
        'Staging is serving a stale or foreign build — refusing to claim same-build evidence. ' +
        'See e2e/README.md.',
    );
  }
}
