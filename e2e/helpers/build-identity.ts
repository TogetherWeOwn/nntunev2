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

/**
 * Outcome of the same-run `staging-api` job, as `API_DEPLOY_OUTCOME`
 * (`deploy.yml` → `e2e-staging.yml` `inputs.api_deploy_outcome`). Empty when
 * the caller does not wire it; the API verdict then stays strict.
 */
export function apiDeployOutcome(): string {
  return (process.env.API_DEPLOY_OUTCOME || '').trim();
}

/**
 * `staging-api` outcomes meaning this run did not redeploy the API on
 * purpose: the job was skipped (no api changes / gate) or staging is not
 * wired. The serving API is then an earlier build and may legitimately
 * differ from head; the verdict says so instead of claiming web=API=head.
 */
const API_NOT_REDEPLOYED: ReadonlySet<string> = new Set(['skipped', 'unconfigured']);

/**
 * Pull the API Worker build SHA out of a `GET /healthz` body.
 *
 * The Worker returns exactly `{ ok: true, build_sha }`, where `build_sha` is
 * a 40-char lowercase hex SHA or `"unknown"`. A body of exactly `{ ok: true }`
 * is an API build that predates identity reporting and returns `null`
 * (the verdict decides whether that is acceptable). Anything else throws.
 */
export function extractApiBuildSha(bodyText: string, context: string): string | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(bodyText) as unknown;
  } catch {
    throw new Error(
      `[${context}] API build identity body is not JSON (${bodyText.length} chars received): ` +
        'expected exactly { ok, build_sha } from GET /healthz. See e2e/README.md.',
    );
  }
  if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) {
    throw new Error(
      `[${context}] API build identity body is not a JSON object: expected exactly ` +
        '{ ok, build_sha } from GET /healthz. See e2e/README.md.',
    );
  }
  const body = parsed as Record<string, unknown>;
  const keys = Object.keys(body).sort();
  if (body.ok !== true) {
    throw new Error(`[${context}] API GET /healthz did not confirm ok:true. See e2e/README.md.`);
  }
  if (keys.length === 1) return null;
  if (keys.length !== 2 || keys[1] !== 'ok' || keys[0] !== 'build_sha') {
    throw new Error(
      `[${context}] API build identity body must carry exactly { ok, build_sha } from ` +
        `GET /healthz, got keys: ${keys.join(', ')}. See e2e/README.md.`,
    );
  }
  if (typeof body.build_sha !== 'string' || body.build_sha.length === 0) {
    throw new Error(
      `[${context}] API build identity build_sha is missing: expected a 40-hex SHA or ` +
        '"unknown" from GET /healthz. See e2e/README.md.',
    );
  }
  return body.build_sha;
}

/**
 * Verdict for the API Worker identity against the promotion SHA.
 *
 * - `staging-api` deployed this run (or the outcome is not wired): strict.
 *   The API must report exactly the promotion SHA; absent, `"unknown"`,
 *   malformed or different throws by name.
 * - `staging-api` skipped/unconfigured: the API was not redeployed, so a
 *   different SHA is legitimate. Always returns `sameBuild: false` (even on
 *   an equal SHA) with a note the caller logs and annotates — never a
 *   silent pass, never a web=API claim.
 * - Any other outcome (failed, cancelled, unexpected): throws.
 */
export function assertApiBuildMatchesPromotion(
  apiSha: string | null,
  expectedSha: string,
  outcome: string,
  context: string,
): { sameBuild: boolean; note: string } {
  const shown = apiSha ?? '(absent)';
  if (API_NOT_REDEPLOYED.has(outcome)) {
    // Never same-run evidence, even when an earlier API deploy happens to
    // report the same SHA (for example a same-SHA rerun).
    return {
      sameBuild: false,
      note:
        `staging-api ${outcome} this run: the API was not redeployed and serves ${shown} ` +
        `(promotion SHA ${expectedSha}); a difference is legitimate. ` +
        'web=API=head is NOT claimed for this run.',
    };
  }
  if (outcome !== '' && outcome !== 'deployed') {
    throw new Error(
      `[${context}] API build identity cannot be claimed: staging-api outcome is ` +
        `"${outcome}" (expected deployed, skipped or unconfigured). See e2e/README.md.`,
    );
  }
  const unwired =
    outcome === ''
      ? ' API_DEPLOY_OUTCOME is not set, so the API is held to the promotion SHA; if ' +
        'staging-api did not deploy this run, wire inputs.api_deploy_outcome from deploy.yml.'
      : '';
  if (apiSha === null) {
    throw new Error(
      `[${context}] API build identity is absent: GET /healthz carries no build_sha ` +
        `(an API build that predates identity reporting), promotion SHA ${expectedSha}.` +
        `${unwired} See e2e/README.md.`,
    );
  }
  if (apiSha === 'unknown') {
    throw new Error(
      `[${context}] API build identity is unknown: GET /healthz reported "unknown" instead ` +
        `of the promotion SHA ${expectedSha} (BUILD_SHA var not set on the Worker).` +
        `${unwired} See e2e/README.md.`,
    );
  }
  if (!SHA40.test(apiSha)) {
    throw new Error(
      `[${context}] API build identity is malformed: GET /healthz reported a build_sha that ` +
        `is neither 40-hex nor "unknown". Promotion SHA ${expectedSha}.${unwired} See e2e/README.md.`,
    );
  }
  if (apiSha !== expectedSha) {
    throw new Error(
      `[${context}] API build identity mismatch: API build ${apiSha} does not match the ` +
        `promotion SHA ${expectedSha} from the push-triggered deploy run.${unwired} ` +
        'See e2e/README.md.',
    );
  }
  return { sameBuild: true, note: '' };
}

/**
 * Turn one `GET /healthz` response into the API identity verdict.
 *
 * When `staging-api` did not redeploy (skipped/unconfigured), a non-200
 * answer (no API Worker route, so the web catch-all answers) records "no
 * API identity" without claiming web=API=head. Every other outcome needs
 * HTTP 200 with the exact `{ ok, build_sha }` shape, judged by
 * `assertApiBuildMatchesPromotion`.
 */
export function resolveApiIdentity(
  status: number,
  bodyText: string,
  expectedSha: string,
  outcome: string,
  context: string,
): { apiSha: string | null; sameBuild: boolean; note: string } {
  if (status !== 200 && API_NOT_REDEPLOYED.has(outcome)) {
    return {
      apiSha: null,
      sameBuild: false,
      note:
        `staging-api ${outcome} this run: API GET /healthz answered HTTP ${status}, ` +
        `so no API identity is available (promotion SHA ${expectedSha}). ` +
        'web=API=head is NOT claimed for this run.',
    };
  }
  if (status !== 200) {
    throw new Error(
      `[${context}] API build identity request failed: expected HTTP 200 from GET /healthz, ` +
        `got HTTP ${status} (body ${bodyText.length} chars, promotion SHA ${expectedSha}). ` +
        'See e2e/README.md.',
    );
  }
  const apiSha = extractApiBuildSha(bodyText, context);
  return { apiSha, ...assertApiBuildMatchesPromotion(apiSha, expectedSha, outcome, context) };
}
