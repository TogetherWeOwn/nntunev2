/**
 * Frontend/API contract the E2E suite drives.
 *
 * The SvelteKit app (sibling card) implements these `data-testid` hooks and
 * JSON endpoints; the specs below are the executable side of that contract.
 * If the app drifts (renamed hook, reshaped body), the failing assertion
 * names the contract key — that failure IS the signal, not noise.
 */

// UI hooks (data-testid) the app must render.
export const TESTIDS = {
  appRoot: 'app-root',
  signupEmail: 'signup-email',
  signupSubmit: 'signup-submit',
  signupError: 'signup-error',
  byopHost: 'byop-host',
  byopPort: 'byop-port',
  byopUser: 'byop-user',
  byopPass: 'byop-pass',
  byopTls: 'byop-tls',
  byopTest: 'byop-test-connection',
  byopSave: 'byop-save',
  byopStatus: 'byop-status',
  importFile: 'import-file',
  importStatus: 'import-status',
  libraryItem: 'library-item',
  libraryList: 'library-list',
  libraryStaleBadge: 'library-stale-badge',
  audioPlayer: 'audio-player',
  usageMeter: 'usage-meter',
  planLimitNotice: 'plan-limit-notice',
} as const;

// API endpoints (relative to STAGING_BASE_URL).
export const API = {
  health: '/api/health',
  ready: '/api/ready',
  // Deployed-build identity surface (`apps/web/src/routes/api/build/+server.ts`,
  // baked SHA from `src/lib/build.ts`). A renamed path fails the
  // build-identity spec with this key named.
  build: '/api/build',
  testAccounts: '/api/test-accounts',
  // Staging-only emailed-code retrieval seam (paired staging-seam build slice):
  // returns the issued signup code for an `example.test` mailbox without
  // minting an account. Fail-closed without the staging flag; absent in
  // production. A renamed path fails the wizard-arrival spec with this key
  // named.
  testCodes: '/api/test-codes',
  ingest: '/api/ingest',
  ingestStatus: (id: string) => `/api/ingest/${id}`,
  library: '/api/library',
  stream: (id: string) => `/api/stream/${id}`,
  usage: '/api/usage',
  // Deterministic no-auth WAV (`apps/web/src/routes/api/demo/audio/+server.ts`,
  // bytes pinned by `helpers/demo-audio.ts`). A renamed path fails the
  // demo-audio specs with this key named.
  demoAudio: '/api/demo/audio',
} as const;

/** HTTP statuses the API uses to signal plan-limit enforcement. */
export const PLAN_LIMIT_STATUSES = [402, 429] as const;

/** Server limit codes that satisfy a plan-limit refusal (docs/api-errors.md). */
export const PLAN_LIMIT_CODES = ['plan_limit_exceeded', 'rate_limited'] as const;

/**
 * A named secret that must never appear in a plan-limit response body.
 * Only the name travels in diagnostics — values stay out of CI artifacts.
 */
export interface NamedSecret {
  name: string;
  value: string;
}

/**
 * Pull the limit code out of a 402/429 body, tolerating the envelope shapes
 * the client already accepts (`{ code }`, `{ error: "code" }`,
 * `{ error: { code } }` — see `paywallFromResponse` in
 * apps/web/src/lib/billing/planLimit.ts). Throws naming the actual keys so
 * a missing or renamed code fails by name instead of passing silently.
 */
export function extractLimitCode(bodyText: string, context: string): string {
  let parsed: unknown;
  try {
    parsed = JSON.parse(bodyText) as unknown;
  } catch {
    throw new Error(
      `[${context}] plan-limit body is not JSON (${bodyText.length} chars received): ` +
        'expected the documented {code, message, retryable} envelope (docs/api-errors.md).',
    );
  }
  if (typeof parsed !== 'object' || parsed === null) {
    throw new Error(
      `[${context}] plan-limit body is not a JSON object, got ${typeof parsed}: ` +
        'expected the documented {code, message, retryable} envelope (docs/api-errors.md).',
    );
  }
  const record = parsed as Record<string, unknown>;
  const candidates: unknown[] = [record.code, record.error];
  const nested = record.error;
  if (typeof nested === 'object' && nested !== null) {
    candidates.push((nested as Record<string, unknown>).code);
  }
  const code = candidates.find(
    (value): value is string =>
      typeof value === 'string' && (PLAN_LIMIT_CODES as readonly string[]).includes(value),
  );
  if (!code) {
    throw new Error(
      `[${context}] plan-limit body names no limit code: expected one of ` +
        `${PLAN_LIMIT_CODES.join(', ')}, got keys: ${Object.keys(record).join(', ') || '(empty)'}.`,
    );
  }
  return code;
}

/**
 * Require the limit + reset headers on a 402/429 refusal. Name matching is
 * deliberately tolerant (any header naming a limit, any naming a reset or
 * retry) so the enforcement card owns the exact wire names; the preferred
 * names are `x-plan-limit` + `x-plan-reset` (402) with `retry-after` already
 * the repo convention for 429 (workers/api/src/rate_limit.rs). A 429 must
 * additionally carry a non-empty `retry-after` even when another reset
 * header is present (docs/api-errors.md). Throws listing the received
 * header names (never values) so a missing header fails by name.
 */
export function assertPlanLimitHeaders(
  headers: Record<string, string>,
  context: string,
  status?: number,
): void {
  const lowered: Record<string, string> = {};
  for (const [name, value] of Object.entries(headers)) lowered[name.toLowerCase()] = value;
  const names = Object.keys(lowered);
  const limitHeader = names.find((name) => name.includes('limit'));
  const resetHeader = names.find((name) => name.includes('reset') || name.includes('retry-after'));
  if (!limitHeader || !resetHeader) {
    throw new Error(
      `[${context}] plan-limit refusal is missing ` +
        `${[!limitHeader && 'a limit header (e.g. x-plan-limit)', !resetHeader && 'a reset/retry header (e.g. x-plan-reset or retry-after)'].filter(Boolean).join(' + ')}: ` +
        `got headers: ${names.join(', ') || '(none)'}.`,
    );
  }
  if (!lowered[limitHeader]?.trim() || !lowered[resetHeader]?.trim()) {
    throw new Error(
      `[${context}] plan-limit headers are present but empty: ` +
        `'${limitHeader}' + '${resetHeader}' must carry values (names only — no values logged).`,
    );
  }
  if (status === 429) {
    const retryAfter = names.find((name) => name === 'retry-after');
    if (!retryAfter || !lowered[retryAfter]?.trim()) {
      throw new Error(
        `[${context}] 429 refusal is missing a non-empty retry-after header ` +
          `(docs/api-errors.md: 429 always carries Retry-After): ` +
          `got headers: ${names.join(', ') || '(none)'}.`,
      );
    }
  }
}

/** Key fragment that points the client at the upgrade path (`/pricing`). */
const UPGRADE_KEY_RE = /upgrade|pricing|cta/i;

/**
 * Value fragment that points the client at the upgrade path: the `/pricing`
 * route itself. A bare word like "upgrade" with no route is marketing
 * prose, not a pointer, and does not satisfy the check.
 */
const UPGRADE_VALUE_RE = /\/pricing/i;

/**
 * Require the upgrade CTA pointer in a 402/429 body: a key naming the
 * upgrade path whose value points at the `/pricing` route (preferred:
 * `upgrade_url: "/pricing"`, see the limit-refusal wire in
 * docs/api-errors.md) or a string value carrying the `/pricing` route. A
 * matching key with a non-route value (`false`, `""`, marketing prose) is
 * not a pointer — the nested value is searched instead. The web paywall
 * client hardcodes its CTA href today (PRICING_HREF in
 * apps/web/src/lib/billing/planLimit.ts); this pointer is the
 * server-advertised half, so any client can route the upgrade without
 * hardcoding it. A bare code with no upgrade path is a dead-end error, not
 * the contracted refusal. Throws naming the actual top-level keys.
 */
export function extractUpgradeCta(body: unknown, context: string): string {
  const seen: string[] = [];
  const visit = (value: unknown, depth: number): string | null => {
    if (depth > 4 || typeof value !== 'object' || value === null) {
      if (typeof value === 'string' && UPGRADE_VALUE_RE.test(value)) return `value ${JSON.stringify(value.slice(0, 60))}`;
      return null;
    }
    for (const [key, entry] of Object.entries(value as Record<string, unknown>)) {
      if (depth === 0) seen.push(key);
      if (UPGRADE_KEY_RE.test(key)) {
        if (typeof entry === 'string' && UPGRADE_VALUE_RE.test(entry)) return `key '${key}'`;
        const nested = visit(entry, depth + 1);
        if (nested) return `key '${key}' -> ${nested}`;
        continue;
      }
      const hit = visit(entry, depth + 1);
      if (hit) return hit;
    }
    return null;
  };
  const hit = visit(body, 0);
  if (!hit) {
    throw new Error(
      `[${context}] plan-limit body carries no upgrade CTA pointer: expected a key ` +
        `naming the upgrade path (e.g. upgrade_url) or a value pointing at /pricing, got keys: ` +
        `${seen.join(', ') || '(empty)'}.`,
    );
  }
  return hit;
}

/** Key names that must never appear in a plan-limit body (credential-shaped). */
const SECRET_KEY_RE = /password|passwd|secret|api[_-]?key|private[_-]?key|session|token|credential|passkey/i;

/**
 * Fail when a plan-limit body leaks secrets: any known secret VALUE in the
 * raw text, or any credential-shaped KEY in the parsed JSON. Diagnostics
 * name only the secret's name (or the offending key) — values are never
 * interpolated, matching the seam-diagnostics contract in access.ts.
 * Returns null when clean.
 */
export function secretLeak(
  bodyText: string,
  secrets: NamedSecret[],
  context: string,
): string | null {
  for (const secret of secrets) {
    if (secret.value && bodyText.includes(secret.value)) {
      return `[${context}] plan-limit body leaks the ${secret.name} (value redacted): refusing to pass with credential material in a limit refusal.`;
    }
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(bodyText) as unknown;
  } catch {
    return null;
  }
  const visit = (value: unknown, depth: number): string | null => {
    if (depth > 4 || typeof value !== 'object' || value === null) return null;
    for (const [key, entry] of Object.entries(value as Record<string, unknown>)) {
      if (SECRET_KEY_RE.test(key)) return key;
      const hit = visit(entry, depth + 1);
      if (hit) return hit;
    }
    return null;
  };
  const key = visit(parsed, 0);
  return key
    ? `[${context}] plan-limit body carries a credential-shaped key '${key}': limit refusals must be names-only (docs/api-errors.md).`
    : null;
}

/**
 * The full plan-limit refusal verdict for one capped-account attempt:
 * status is 402/429, limit + reset headers are present (429 additionally
 * requires `retry-after`), the body names the status-consistent limit code
 * (`plan_limit_exceeded` on 402, `rate_limited` on 429) with a present,
 * boolean, status-consistent retryable flag, carries the upgrade CTA
 * pointer, and leaks no secrets. Every failure names the gap (expected vs
 * actual status, missing header names, body keys) so a run without
 * enforcement fails with the missing piece named — never a bare mismatch
 * and never a skip.
 */
export function assertPlanLimitRefusal(args: {
  status: number;
  headers: Record<string, string>;
  bodyText: string;
  secrets: NamedSecret[];
  context: string;
}): void {
  const { status, headers, bodyText, secrets, context } = args;
  if (!(PLAN_LIMIT_STATUSES as readonly number[]).includes(status)) {
    throw new Error(
      `[${context}] expected a plan-limit status (${PLAN_LIMIT_STATUSES.join('/')}), got HTTP ${status}. ` +
        'Without enforcement the capped account streams/ingests as normal; with enforcement it is refused.',
    );
  }
  assertPlanLimitHeaders(headers, context, status);
  const code = extractLimitCode(bodyText, context);
  const expectedCode = status === 429 ? 'rate_limited' : 'plan_limit_exceeded';
  if (code !== expectedCode) {
    throw new Error(
      `[${context}] plan-limit body code '${code}' disagrees with the status: ` +
        `HTTP ${status} requires code '${expectedCode}' ` +
        `(docs/api-errors.md: 402 is plan_limit_exceeded, 429 is rate_limited).`,
    );
  }
  let parsed: unknown = null;
  try {
    parsed = JSON.parse(bodyText) as unknown;
  } catch {
    // extractLimitCode already threw above; unreachable.
  }
  const flag =
    typeof parsed === 'object' && parsed !== null
      ? (parsed as Record<string, unknown>).retryable
      : undefined;
  {
    const expected = status === 429;
    if (typeof flag !== 'boolean' || flag !== expected) {
      throw new Error(
        `[${context}] plan-limit body retryable flag disagrees with the status: ` +
          `HTTP ${status} requires retryable=${expected} (docs/api-errors.md: 402 never retryable, 429 always), ` +
          `body code '${code}'.`,
      );
    }
  }
  extractUpgradeCta(parsed, context);
  const leak = secretLeak(bodyText, secrets, context);
  if (leak) throw new Error(leak);
}

/**
 * Ports the BYOP UI offers; the fixture transport must stay inside this
 * allowlist. Lives here (not access.ts, which reads `process.env`) so the
 * web contract test can pin it against ALLOWED_NNTP_PORTS
 * (vault-validation.ts, itself mirroring vault.rs) without dragging Node
 * types into the web tsconfig — see apps/web/src/lib/contract.test.ts.
 */
export const SUPPORTED_NNTP_PORTS: readonly number[] = [119, 563];

/** A fresh, collision-free test mailbox per run (example.test never routes). */
export function uniqueTestEmail(prefix = 'e2e'): string {
  const stamp = new Date().toISOString().replace(/[-:.TZ]/g, '');
  const rand = Math.floor(Math.random() * 1_000_000)
    .toString()
    .padStart(6, '0');
  return `${prefix}+${stamp}-${rand}@example.test`;
}

/**
 * Pull a streamed-bytes counter out of a usage body, tolerating field-name
 * drift by checking the known keys. Throws listing the actual keys so a
 * contract change shows up as an explicit diff, not a silent zero.
 */
export function extractUsageBytes(body: unknown): number {
  if (typeof body !== 'object' || body === null) {
    throw new Error(`[usage] expected a JSON object, got ${typeof body}.`);
  }
  const record = body as Record<string, unknown>;
  const candidates = ['bytes_streamed', 'streamed_bytes', 'usage_bytes', 'used_bytes'];
  for (const key of candidates) {
    const value = record[key];
    if (typeof value === 'number' && Number.isFinite(value)) return value;
  }
  // One level of common nesting ({ usage: {...} } / { data: {...} }).
  for (const nest of ['usage', 'data']) {
    const inner = record[nest];
    if (typeof inner === 'object' && inner !== null) {
      try {
        return extractUsageBytes(inner);
      } catch {
        // fall through to the contract error below
      }
    }
  }
  throw new Error(
    `[usage] no known byte counter (${candidates.join(', ')}) in response keys: ` +
      `${Object.keys(record).join(', ') || '(empty)'}. ` +
      'If the metering card renamed the field, update extractUsageBytes + this spec together.',
  );
}

/** Pull plan-limit metadata ({ limit, used, tier }) tolerating key drift. */
export function extractPlanMeta(body: unknown): { limit: number; used: number; tier: string } {
  if (typeof body !== 'object' || body === null) {
    throw new Error(`[usage] expected a JSON object for plan metadata, got ${typeof body}.`);
  }
  const record = body as Record<string, unknown>;
  const pick = (keys: string[]): unknown => {
    for (const key of keys) {
      if (record[key] !== undefined) return record[key];
    }
    const inner = (record.usage ?? record.data) as Record<string, unknown> | undefined;
    if (typeof inner === 'object' && inner !== null) {
      for (const key of keys) {
        if (inner[key] !== undefined) return inner[key];
      }
    }
    return undefined;
  };
  const limit = pick(['limit_bytes', 'plan_limit_bytes', 'limit']);
  const used = pick(['used_bytes', 'usage_bytes', 'used']);
  const tier = pick(['plan', 'plan_tier', 'tier']);
  if (typeof limit !== 'number' || typeof used !== 'number' || typeof tier !== 'string') {
    throw new Error(
      '[usage] plan metadata incomplete: expected numeric limit/used + string tier, ' +
        `got keys: ${Object.keys(record).join(', ') || '(empty)'}.`,
    );
  }
  return { limit, used, tier };
}

/** Canonical MBID shape: 8-4-4-4-12 hex (mirrors validate_mbid in workers/ingest/src/queue.rs). */
const MBID_RE = /^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/;

/** True for a canonical 8-4-4-4-12 hex MBID string. */
export function isMbid(value: unknown): value is string {
  return typeof value === 'string' && MBID_RE.test(value);
}

/**
 * Normalize a /api/library payload into its track rows. Accepts either a
 * bare array (the documented shape) or `{ items: [...] }` (the shape the
 * library view also renders), and drops entries without a string `id` so
 * one malformed row can never blank the list. Throws naming the actual
 * shape so a contract change shows up as an explicit diff, not a silent
 * empty list.
 */
export function extractLibraryItems(bodyText: string): Array<Record<string, unknown>> {
  let parsed: unknown;
  try {
    parsed = JSON.parse(bodyText) as unknown;
  } catch {
    throw new Error(
      '[library] library payload is not JSON ' +
        `(${bodyText.length} chars received). If the library card changed the encoding, ` +
        'update extractLibraryItems + this spec together.',
    );
  }
  const raw = Array.isArray(parsed)
    ? parsed
    : typeof parsed === 'object' && parsed !== null
      ? (parsed as Record<string, unknown>).items
      : undefined;
  if (!Array.isArray(raw)) {
    const keys =
      typeof parsed === 'object' && parsed !== null
        ? Object.keys(parsed as Record<string, unknown>).join(', ') || '(empty)'
        : typeof parsed;
    throw new Error(
      '[library] expected a bare array or { items: [...] } from /api/library, ' +
        `got keys: ${keys}. If the library card reshaped the payload, ` +
        'update extractLibraryItems + this spec together.',
    );
  }
  return raw.filter(
    (entry): entry is Record<string, unknown> =>
      typeof entry === 'object' &&
      entry !== null &&
      typeof (entry as Record<string, unknown>).id === 'string' &&
      ((entry as Record<string, unknown>).id as string).length > 0,
  );
}

/**
 * Pull the ingested track's recording MBIDs out of one /api/library row.
 * The release-to-recordings route (#143) fills `recording_mbids` on the
 * library write, so a matched ingest must carry a non-empty list here.
 * Throws naming the track's actual keys (and the release state) so a
 * missing fill fails by name instead of passing silently: a absent field
 * is an exposure gap, an empty list on a release-less row is the
 * by-design unmatched write (see `library_write_for` in
 * workers/ingest/src/matching.rs), and an empty list beside a release
 * is a dropped recordings fetch.
 */
export function extractRecordingMbids(item: unknown): string[] {
  if (typeof item !== 'object' || item === null) {
    throw new Error(`[recording-mbids] expected a library track object, got ${typeof item}.`);
  }
  const record = item as Record<string, unknown>;
  const raw = record.recording_mbids ?? record.recordingMbids;
  if (raw === undefined) {
    throw new Error(
      '[recording-mbids] library track carries no recording MBIDs: expected `recording_mbids` ' +
        "(or the 'recordingMbids' alias), got keys: " +
        `${Object.keys(record).join(', ') || '(empty)'}. ` +
        'If the library card renamed the field, update extractRecordingMbids + this spec together.',
    );
  }
  if (!Array.isArray(raw)) {
    throw new Error(
      `[recording-mbids] expected recording_mbids to be an array, got ${typeof raw}.`,
    );
  }
  if (raw.length === 0) {
    const release =
      typeof record.release_mbid === 'string' && record.release_mbid.length > 0
        ? `release_mbid ${record.release_mbid}`
        : typeof record.releaseMbid === 'string' && (record.releaseMbid as string).length > 0
          ? `releaseMbid ${record.releaseMbid as string}`
          : 'no release_mbid on the row';
    throw new Error(
      '[recording-mbids] library track carries an empty recording_mbids list ' +
        `(${release}). An unmatched ingest writes empty by design; a matched release with ` +
        'no recordings is a dropped recordings fetch (see recording_mbids_or_fallback in ' +
        'workers/ingest/src/matching.rs).',
    );
  }
  const bad = raw.findIndex((entry) => !isMbid(entry));
  if (bad !== -1) {
    const entry = raw[bad];
    throw new Error(
      `[recording-mbids] recording_mbids[${bad}] is not a canonical 8-4-4-4-12 hex MBID, ` +
        `got ${typeof entry === 'string' ? JSON.stringify(entry).slice(0, 60) : typeof entry}.`,
    );
  }
  return raw as string[];
}
