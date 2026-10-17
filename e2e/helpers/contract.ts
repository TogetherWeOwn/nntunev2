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
  testAccounts: '/api/test-accounts',
  ingest: '/api/ingest',
  ingestStatus: (id: string) => `/api/ingest/${id}`,
  library: '/api/library',
  stream: (id: string) => `/api/stream/${id}`,
  usage: '/api/usage',
} as const;

/** HTTP statuses the API uses to signal plan-limit enforcement. */
export const PLAN_LIMIT_STATUSES = [402, 429] as const;

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

/** Pull plan-limit metadata ({ limit, used, tier }) tolerating key drift. */export function extractPlanMeta(body: unknown): { limit: number; used: number; tier: string } {
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
