/**
 * Promotion-readiness gate for the staging smoke.
 *
 * The readiness route may legitimately answer 503 `{ok:false}` with
 * `Retry-After` when a downstream check is not ready — that is a well-formed
 * not-ready answer, and this helper does not change it. Promotion is
 * stricter: a build is only promotion-ready when readiness answers
 * HTTP 200 with JSON `{ok:true}` right now. Everything else (404 from a
 * build predating the route, 503, an HTML 200 app shell, malformed JSON,
 * `{ok:false}`, a non-boolean `ok`) must fail the smoke, never green it.
 *
 * Pure: no network, no environment reads, and it never skips. Callers probe
 * for the Cloudflare Access wall first (`assertNoAccessWall`) so a wall
 * response is reported as a wall, not as a readiness failure.
 */

import type { WallProbe } from './access.js';

/**
 * Throw unless `probe` is a promotion-ready readiness response:
 * status exactly 200, body parses as JSON, and the parsed value is an
 * object whose `ok` is strictly boolean `true`.
 */
export function assertPromotionReady(probe: WallProbe, context: string): void {
  // Never interpolate probe.url: the message lands in uploaded test
  // artifacts, so it carries only fixed context, status and reason —
  // never the staging origin or path.
  const fail = (reason: string): Error =>
    new Error(
      `[${context}] promotion-readiness failed ` +
        `(HTTP ${probe.status}): ${reason} ` +
        'Promotion requires HTTP 200 JSON {ok:true}. See e2e/README.md.',
    );

  if (probe.status !== 200) {
    throw fail('readiness did not return HTTP 200.');
  }

  let parsed: unknown;
  try {
    parsed = JSON.parse(probe.bodyText);
  } catch {
    const contentType =
      probe.headers['content-type'] ?? probe.headers['Content-Type'] ?? 'unknown';
    throw fail(`body is not valid JSON (content-type: ${contentType}).`);
  }

  if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) {
    throw fail('JSON body is not an object.');
  }
  if ((parsed as Record<string, unknown>).ok !== true) {
    throw fail('JSON body does not carry ok:true (strictly boolean).');
  }
}
