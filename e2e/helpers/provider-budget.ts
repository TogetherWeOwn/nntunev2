/**
 * Staging provider connection budget for the post-deploy journey.
 *
 * The staging NNTP provider is a shared account (production traffic uses the
 * same credentials), so every E2E run holds at most a trickle of connections:
 * the workflow concurrency group (`e2e-staging`, cancel-in-progress: false)
 * already serializes provider runs, and this budget caps each run at 2 by
 * default, read from the environment with a hard upper bound of 10 (the
 * account allows at least 10). Legs that fetch provider bytes must stay
 * within the budget, fetch only the segments they need, and report peak
 * connections + bytes fetched in the evidence comment.
 *
 * Owner rule (public repo): secret names only, never values — and no NZB
 * content of any kind. This file carries only the env-var name and numbers.
 */

import { randomUUID } from 'node:crypto';

/** Environment variable carrying the per-run provider connection cap. */
export const PROVIDER_BUDGET_ENV = 'E2E_NNTP_MAX_CONNECTIONS';

/** Default cap: 2 connections per run — the shared account keeps 8+ free for production. */
export const PROVIDER_BUDGET_DEFAULT = 2;

/**
 * Hard upper bound: 10. The provider allows at least 10 connections, but a
 * run never needs more than a trickle — anything above 10 is clamped, and a
 * non-numeric or non-positive value falls back to the default.
 */
export const PROVIDER_BUDGET_HARD_MAX = 10;

export interface ProviderBudget {
  /** Effective cap after clamping (always 1..PROVIDER_BUDGET_HARD_MAX). */
  maxConnections: number;
  /** Raw value read from the environment (before clamping), or null when unset. */
  requested: number | null;
  /** True when the effective cap differs from what was requested. */
  clamped: boolean;
}

/**
 * Read the per-run provider connection cap from the environment. Pure (the
 * env mapping is injected) so offline self-tests pin the parse/clamp rules
 * without touching the process environment or the network.
 */
export function readProviderBudget(
  env: Record<string, string | undefined>,
): ProviderBudget {
  const raw = env[PROVIDER_BUDGET_ENV];
  if (raw === undefined) {
    return { maxConnections: PROVIDER_BUDGET_DEFAULT, requested: null, clamped: false };
  }
  const parsed = Number(raw.trim());
  if (!Number.isInteger(parsed) || parsed <= 0) {
    return { maxConnections: PROVIDER_BUDGET_DEFAULT, requested: null, clamped: true };
  }
  if (parsed > PROVIDER_BUDGET_HARD_MAX) {
    return { maxConnections: PROVIDER_BUDGET_HARD_MAX, requested: parsed, clamped: true };
  }
  return { maxConnections: parsed, requested: parsed, clamped: false };
}

/**
 * Mint a never-posted probe message-ID for the 430 control leg. Random per
 * run under the reserved `.invalid` TLD, which can never resolve to a real
 * posting — the control asserts the provider answers 430 without fetching
 * any NZB content. Own ID only, never NZB content, so it is safe to count
 * (but never its value) in evidence.
 */
export function mintProbeMessageId(): string {
  return `<e2e-430-probe-${randomUUID()}@nntune.invalid>`;
}

/**
 * True iff the NNTP reply line carries exactly the expected code: the code
 * itself followed by a space or continuation dash. A bare prefix never
 * matches (`43` is not `430`), and a code with nothing after it never
 * matches either — every on-the-wire reply carries text or a dash.
 */
export function isNntpCode(line: string, code: string): boolean {
  return (
    line.length > code.length &&
    line.startsWith(code) &&
    (line[code.length] === ' ' || line[code.length] === '-')
  );
}
