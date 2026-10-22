/**
 * Bot-contract conformance checker (spec section 5): offline verdicts over
 * the JSON corpus in `e2e/fixtures/bot-contract/`.
 *
 * Three layers, each feeding the B8 staging harness:
 *
 * - grant envelope (`checkGrantVector`): vendored mirror of
 *   `workers/stream/src/grant.rs` (`verify` order, HMAC-SHA256 composition,
 *   TTL cap + skew). Proves byte-shape conformance of `v1` tokens.
 * - signed stream URL (`checkSignedUrlVector`): the banked
 *   `https://<host>/play/<track>?grant=<token>[&profile=opus-64]` shape
 *   (see the corpus README for the banked choices).
 * - verdict matrix (`checkMatrixRow`): one bot byte-fetch attempt maps to
 *   the wire verdict (status + typed code + retryable) under the banked
 *   precedence, mirroring `PlayError::api_error` in
 *   `workers/stream/src/play.rs` and the scope map in
 *   `workers/api/src/native_rest.rs`.
 *
 * Failure reasons are checker-internal snake-case tokens, never wire codes:
 * the corpus asserts them so a mutation fails by name. Every grant rejection
 * maps to the generic 401 `unauthorized` on the wire (the matrix layer pins
 * that); the reason here names which check fired for diagnostics only.
 *
 * Crypto runs on `node:crypto` (offline, no staging, no network). Fixture
 * secrets resolve by NAME (`CORPUS_SECRETS`); values never appear in
 * fixtures, diagnostics, or failure messages.
 */

import { createHash, createHmac, timingSafeEqual } from 'node:crypto';

/** Stable contract IDs. Pinned against the Rust sources by `tests/bot-contract.spec.ts`. */
export const BOT_CONTRACT = {
  /** The one transcode profile (`PROFILE_NAME` in `crates/nntune-core/src/transcode.rs`). */
  profileName: 'opus-64',
  /** Requested profile when `?profile=` is absent: the verified source bytes. */
  sourceProfile: 'source',
  /** `track_media.format` for opus-64 artifacts. */
  artifactFormat: 'opus',
  /** `track_media.bitrate` for opus-64 artifacts (kbps). */
  artifactBitrateKbps: 64,
  /** Artifact sample rate (Hz). */
  artifactSampleRateHz: 48000,
  /** Artifact channel count. */
  artifactChannels: 2,
  /** Grant lifetime cap (`MAX_GRANT_TTL` in `workers/stream/src/grant.rs`). */
  maxGrantTtlSecs: 900,
  /** Verifier skew (`CLOCK_SKEW_SECS` in `workers/stream/src/grant.rs`). */
  clockSkewSecs: 60,
  /** Scope the stream route requires (`SCOPE_STREAM_READ`). */
  streamReadScope: 'stream:read',
  /** Grant envelope version this checker verifies. */
  grantVersion: 'v1',
  /** Corpus `version` this checker reads; anything else fails loud. */
  corpusVersion: 1,
} as const;

/**
 * Fixture secrets by name. Synthetic and offline-only: these mint and verify
 * corpus tokens in tests and must never authenticate a real environment.
 * Fixtures name a key; only the name travels in diagnostics.
 */
const CORPUS_SECRETS: Record<string, string> = {
  corpus: 'bot-contract-corpus-secret-please-ignore',
};

/**
 * Typed-code registry mirror for the codes the matrix uses. Pinned against
 * `docs/api-errors.md` by `tests/bot-contract.spec.ts` (same mirror +
 * doc-lint shape as the per-worker `api_error` registries).
 */
export const ERROR_REGISTRY: Record<string, { status: number; retryable: boolean }> = {
  unauthorized: { status: 401, retryable: false },
  provider_auth_expired: { status: 401, retryable: false },
  forbidden: { status: 403, retryable: false },
  not_found: { status: 404, retryable: false },
  invalid_request: { status: 400, retryable: false },
  range_not_satisfiable: { status: 416, retryable: false },
};

export type CheckOk<T> = { ok: true; value: T };
export type CheckFail = { ok: false; reason: string };
export type Check<T> = CheckOk<T> | CheckFail;

export function failed(reason: string): CheckFail {
  return { ok: false, reason };
}

function fail<T>(reason: string): Check<T> {
  return { ok: false, reason };
}

/** sha256 hex of `data` (the `sha256("abc")` vector pins the primitive). */
export function sha256Hex(data: string): string {
  return createHash('sha256').update(data, 'utf8').digest('hex');
}

/** HMAC-SHA256 of `message` under `secret`, base64url without padding. */
export function hmacSha256Base64Url(secret: string, message: string): string {
  return createHmac('sha256', secret).update(message, 'ascii').digest('base64url');
}

function isBase64UrlChar(code: number): boolean {
  return (
    (code >= 0x41 && code <= 0x5a) || // A-Z
    (code >= 0x61 && code <= 0x7a) || // a-z
    (code >= 0x30 && code <= 0x39) || // 0-9
    code === 0x2d || // -
    code === 0x5f // _
  );
}

/**
 * Strict unpadded-base64url gate mirroring `base64url_decode` in
 * `workers/stream/src/grant.rs`: nonempty ASCII, no length-1-mod-4 input,
 * alphabet only, zero padding bits on a short final quantum. Node's own
 * decoder is lenient, so this gate runs first and the decode after it.
 */
export function strictBase64UrlDecode(s: string): Buffer | null {
  if (s.length === 0) return null;
  for (let i = 0; i < s.length; i += 1) {
    const code = s.charCodeAt(i);
    if (code > 0x7f || !isBase64UrlChar(code)) return null;
  }
  if (s.length % 4 === 1) return null;
  const tail = s.length % 4;
  if (tail === 2 || tail === 3) {
    // The final quantum's padding bits must be zero: the last char of a
    // 2-char tail carries 4 pad bits (low 4 of its 6 must be 0 -> one of
    // A,Q,g,w), of a 3-char tail 2 pad bits (low 2 must be 0 -> index % 4).
    const last = s.charCodeAt(s.length - 1);
    const index =
      last >= 0x41 && last <= 0x5a
        ? last - 0x41
        : last >= 0x61 && last <= 0x7a
          ? last - 0x61 + 26
          : last >= 0x30 && last <= 0x39
            ? last - 0x30 + 52
            : last === 0x2d
              ? 62
              : 63;
    const padMask = tail === 2 ? 0b1111 : 0b11;
    if ((index & padMask) !== 0) return null;
  }
  return Buffer.from(s, 'base64url');
}

function constantTimeEqual(a: Buffer, b: Buffer): boolean {
  if (a.length !== b.length) return false;
  return timingSafeEqual(a, b);
}

/** A verified grant: the parsed `user_id|track_id|exp_unix` payload. */
export interface ParsedGrant {
  userId: string;
  trackId: string;
  expUnix: number;
}

export type GrantRejectReason = 'malformed' | 'bad-signature' | 'expired' | 'ttl-too-long';

/**
 * Verify one `v1` token, following `grant::verify` arm order exactly:
 * envelope shape -> signature bytes -> signature match -> payload bytes ->
 * payload shape -> expiry -> TTL cap. Returns the parsed grant or the
 * reason the token was rejected.
 */
export function verifyGrant(
  token: string,
  secretName: string,
  nowUnix: number,
): Check<ParsedGrant> {
  const secret = CORPUS_SECRETS[secretName];
  if (secret === undefined) return fail(`unknown-secret:${secretName}`);
  const rest = token.startsWith(`${BOT_CONTRACT.grantVersion}.`)
    ? token.slice(BOT_CONTRACT.grantVersion.length + 1)
    : null;
  if (rest === null) return fail<ParsedGrant>('malformed');
  const dot = rest.indexOf('.');
  if (dot === -1) return fail<ParsedGrant>('malformed');
  const payloadB64 = rest.slice(0, dot);
  const sigB64 = rest.slice(dot + 1);
  if (payloadB64.length === 0 || sigB64.length === 0) return fail<ParsedGrant>('malformed');
  // A second dot hides inside the signature segment: reject before decode.
  if (sigB64.includes('.')) return fail<ParsedGrant>('malformed');
  const sig = strictBase64UrlDecode(sigB64);
  if (sig === null) return fail<ParsedGrant>('malformed');
  const expectedB64 = hmacSha256Base64Url(secret, payloadB64);
  const expected = Buffer.from(expectedB64, 'base64url');
  if (!constantTimeEqual(sig, expected)) return fail<ParsedGrant>('bad-signature');
  const payloadBytes = strictBase64UrlDecode(payloadB64);
  if (payloadBytes === null) return fail<ParsedGrant>('malformed');
  let payload: string;
  try {
    payload = new TextDecoder('utf-8', { fatal: true }).decode(payloadBytes);
  } catch {
    return fail<ParsedGrant>('malformed');
  }
  const parts = payload.split('|');
  if (parts.length !== 3) return fail<ParsedGrant>('malformed');
  const [userId, trackId, expRaw] = parts as [string, string, string];
  if (!/^[0-9]+$/.test(expRaw)) return fail<ParsedGrant>('malformed');
  const expUnix = Number(expRaw);
  if (!Number.isSafeInteger(expUnix)) return fail<ParsedGrant>('malformed');
  if (nowUnix >= expUnix) return fail<ParsedGrant>('expired');
  if (expUnix - nowUnix > BOT_CONTRACT.maxGrantTtlSecs + BOT_CONTRACT.clockSkewSecs) {
    return fail<ParsedGrant>('ttl-too-long');
  }
  return { ok: true, value: { userId, trackId, expUnix } };
}

export interface GrantVector {
  id: string;
  token: string;
  secret: string;
  now_unix: number;
  expect:
    | { ok: true; user_id: string; track_id: string; exp_unix: number }
    | { ok: false; reason: GrantRejectReason };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function decodeGrantVector(raw: unknown): Check<GrantVector> {
  if (!isRecord(raw)) return fail('corpus-error: grant vector is not an object');
  const { id, token, secret, now_unix: nowUnix, expect } = raw;
  if (typeof id !== 'string' || id.length === 0) return fail('corpus-error: grant vector id');
  if (typeof token !== 'string') return fail(`corpus-error: ${id} token is not a string`);
  if (typeof secret !== 'string') return fail(`corpus-error: ${id} secret is not a string`);
  if (typeof nowUnix !== 'number' || !Number.isSafeInteger(nowUnix) || nowUnix < 0) {
    return fail(`corpus-error: ${id} now_unix is not a non-negative safe integer`);
  }
  if (!isRecord(expect) || typeof expect.ok !== 'boolean') {
    return fail(`corpus-error: ${id} expect.ok is not a boolean`);
  }
  if (expect.ok) {
    const { user_id: userId, track_id: trackId, exp_unix: expUnix } = expect;
    if (typeof userId !== 'string' || typeof trackId !== 'string') {
      return fail(`corpus-error: ${id} expect user/track are not strings`);
    }
    if (typeof expUnix !== 'number' || !Number.isSafeInteger(expUnix) || expUnix < 0) {
      return fail(`corpus-error: ${id} expect exp_unix is not a non-negative safe integer`);
    }
    return {
      ok: true,
      value: { id, token, secret, now_unix: nowUnix, expect: { ok: true, user_id: userId, track_id: trackId, exp_unix: expUnix } },
    };
  }
  const { reason } = expect;
  if (reason !== 'malformed' && reason !== 'bad-signature' && reason !== 'expired' && reason !== 'ttl-too-long') {
    return fail(`corpus-error: ${id} expect.reason is not a grant reject reason`);
  }
  return { ok: true, value: { id, token, secret, now_unix: nowUnix, expect: { ok: false, reason } } };
}

/**
 * Check one grant-envelope vector: the token must verify (or reject with
 * the named reason) under the named fixture secret at `now_unix`.
 */
export function checkGrantVector(raw: unknown): Check<{ id: string }> {
  const decoded = decodeGrantVector(raw);
  if (!decoded.ok) return decoded;
  const vec = decoded.value;
  const result = verifyGrant(vec.token, vec.secret, vec.now_unix);
  if (vec.expect.ok) {
    if (!result.ok) return fail(`${vec.id}: expected verify, got ${result.reason}`);
    const grant = result.value;
    if (grant.userId !== vec.expect.user_id || grant.trackId !== vec.expect.track_id || grant.expUnix !== vec.expect.exp_unix) {
      return fail(`${vec.id}: verified grant differs from expect (names only, no token echoed)`);
    }
    return { ok: true, value: { id: vec.id } };
  }
  if (result.ok) return fail(`${vec.id}: expected reject ${vec.expect.reason}, token verified`);
  if (result.reason !== vec.expect.reason) {
    return fail(`${vec.id}: expected reject ${vec.expect.reason}, got ${result.reason}`);
  }
  return { ok: true, value: { id: vec.id } };
}

/** A verified play URL: the bound track, profile, and grant identity. */
export interface ParsedPlayUrl {
  trackId: string;
  profile: string;
  userId: string;
  expUnix: number;
}

export type UrlRejectReason =
  | 'bad-url'
  | 'not-https'
  | 'bad-path'
  | 'missing-grant'
  | 'duplicate-grant'
  | 'duplicate-profile'
  | 'unknown-profile'
  | 'grant-malformed'
  | 'grant-bad-signature'
  | 'grant-expired'
  | 'grant-ttl-too-long'
  | 'grant-wrong-track';

/**
 * Verify one signed play URL: https shape, `/play/<track>` path, exactly
 * one grant, at most one profile, then grant verification and track
 * binding. Extra unknown query params are ignored (forward-compatible).
 */
export function verifySignedUrl(
  url: string,
  secretName: string,
  nowUnix: number,
): Check<ParsedPlayUrl> {
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    return fail<ParsedPlayUrl>('bad-url');
  }
  if (parsed.protocol !== 'https:') return fail<ParsedPlayUrl>('not-https');
  if (parsed.hostname.length === 0) return fail<ParsedPlayUrl>('not-https');
  const segments = parsed.pathname.split('/');
  if (segments.length !== 3 || segments[0] !== '' || segments[1] !== 'play' || segments[2] === '') {
    return fail<ParsedPlayUrl>('bad-path');
  }
  const trackId = segments[2] as string;
  const grants = parsed.searchParams.getAll('grant');
  if (grants.length === 0) return fail<ParsedPlayUrl>('missing-grant');
  if (grants.length > 1) return fail<ParsedPlayUrl>('duplicate-grant');
  const profiles = parsed.searchParams.getAll('profile');
  if (profiles.length > 1) return fail<ParsedPlayUrl>('duplicate-profile');
  const profileParam = profiles.length === 0 ? null : (profiles[0] as string);
  if (profileParam !== null && profileParam !== BOT_CONTRACT.profileName) {
    return fail<ParsedPlayUrl>('unknown-profile');
  }
  const grant = verifyGrant(grants[0] as string, secretName, nowUnix);
  if (!grant.ok) {
    const reason = grant.reason as GrantRejectReason | string;
    if (reason === 'malformed') return fail<ParsedPlayUrl>('grant-malformed');
    if (reason === 'bad-signature') return fail<ParsedPlayUrl>('grant-bad-signature');
    if (reason === 'expired') return fail<ParsedPlayUrl>('grant-expired');
    if (reason === 'ttl-too-long') return fail<ParsedPlayUrl>('grant-ttl-too-long');
    return fail<ParsedPlayUrl>(`corpus-error: ${reason}`);
  }
  if (grant.value.trackId !== trackId) return fail<ParsedPlayUrl>('grant-wrong-track');
  return {
    ok: true,
    value: {
      trackId,
      profile: profileParam ?? BOT_CONTRACT.sourceProfile,
      userId: grant.value.userId,
      expUnix: grant.value.expUnix,
    },
  };
}

export interface SignedUrlVector {
  id: string;
  url: string;
  secret: string;
  now_unix: number;
  expect:
    | { ok: true; track_id: string; profile: string; user_id: string; exp_unix: number }
    | { ok: false; reason: UrlRejectReason };
}

const URL_REASONS: ReadonlySet<string> = new Set([
  'bad-url',
  'not-https',
  'bad-path',
  'missing-grant',
  'duplicate-grant',
  'duplicate-profile',
  'unknown-profile',
  'grant-malformed',
  'grant-bad-signature',
  'grant-expired',
  'grant-ttl-too-long',
  'grant-wrong-track',
]);

function decodeSignedUrlVector(raw: unknown): Check<SignedUrlVector> {
  if (!isRecord(raw)) return fail('corpus-error: signed-url vector is not an object');
  const { id, url, secret, now_unix: nowUnix, expect } = raw;
  if (typeof id !== 'string' || id.length === 0) return fail('corpus-error: signed-url vector id');
  if (typeof url !== 'string') return fail(`corpus-error: ${id} url is not a string`);
  if (typeof secret !== 'string') return fail(`corpus-error: ${id} secret is not a string`);
  if (typeof nowUnix !== 'number' || !Number.isSafeInteger(nowUnix) || nowUnix < 0) {
    return fail(`corpus-error: ${id} now_unix is not a non-negative safe integer`);
  }
  if (!isRecord(expect) || typeof expect.ok !== 'boolean') {
    return fail(`corpus-error: ${id} expect.ok is not a boolean`);
  }
  if (expect.ok) {
    const { track_id: trackId, profile, user_id: userId, exp_unix: expUnix } = expect;
    if (typeof trackId !== 'string' || typeof profile !== 'string' || typeof userId !== 'string') {
      return fail(`corpus-error: ${id} expect track/profile/user are not strings`);
    }
    if (typeof expUnix !== 'number' || !Number.isSafeInteger(expUnix) || expUnix < 0) {
      return fail(`corpus-error: ${id} expect exp_unix is not a non-negative safe integer`);
    }
    return {
      ok: true,
      value: { id, url, secret, now_unix: nowUnix, expect: { ok: true, track_id: trackId, profile, user_id: userId, exp_unix: expUnix } },
    };
  }
  const { reason } = expect;
  if (typeof reason !== 'string' || !URL_REASONS.has(reason)) {
    return fail(`corpus-error: ${id} expect.reason is not a url reject reason`);
  }
  return { ok: true, value: { id, url, secret, now_unix: nowUnix, expect: { ok: false, reason: reason as UrlRejectReason } } };
}

/** Check one signed-URL vector against its expectation. */
export function checkSignedUrlVector(raw: unknown): Check<{ id: string }> {
  const decoded = decodeSignedUrlVector(raw);
  if (!decoded.ok) return decoded;
  const vec = decoded.value;
  const result = verifySignedUrl(vec.url, vec.secret, vec.now_unix);
  if (vec.expect.ok) {
    if (!result.ok) return fail(`${vec.id}: expected verify, got ${result.reason}`);
    const parsed = result.value;
    if (
      parsed.trackId !== vec.expect.track_id ||
      parsed.profile !== vec.expect.profile ||
      parsed.userId !== vec.expect.user_id ||
      parsed.expUnix !== vec.expect.exp_unix
    ) {
      return fail(`${vec.id}: verified URL differs from expect (names only, no grant echoed)`);
    }
    return { ok: true, value: { id: vec.id } };
  }
  if (result.ok) return fail(`${vec.id}: expected reject ${vec.expect.reason}, URL verified`);
  if (result.reason !== vec.expect.reason) {
    return fail(`${vec.id}: expected reject ${vec.expect.reason}, got ${result.reason}`);
  }
  return { ok: true, value: { id: vec.id } };
}

/** Range resolution mirroring `RangeOutcome` in `workers/stream/src/range.rs`. */
export type RangeVerdict =
  | { kind: 'full' }
  | { kind: 'partial'; start: bigint; end: bigint }
  | { kind: 'unsatisfiable' };

function parseRangeDigits(s: string): bigint | null {
  if (s.length === 0 || !/^[0-9]+$/.test(s)) return null;
  try {
    const value = BigInt(s);
    // Mirror u64: numerals past u64::MAX fail the parse, they are not
    // clamped, so they fall back to a full body exactly like `range.rs`.
    if (value > 18446744073709551615n) return null;
    return value;
  } catch {
    return null;
  }
}

/**
 * Resolve a `Range` header value for a body of `len` bytes, mirroring
 * `range::parse` exactly (single `bytes=` ranges only; anything else is a
 * full body; unsatisfiable resolves to 416).
 */
export function resolveRange(header: string | null, len: number): RangeVerdict {
  const body = BigInt(len);
  if (header === null) return { kind: 'full' };
  const trimmed = header.trim();
  if (!trimmed.startsWith('bytes=')) return { kind: 'full' };
  const spec = trimmed.slice('bytes='.length);
  if (spec.includes(',')) return { kind: 'full' };
  const dash = spec.indexOf('-');
  if (dash === -1) return { kind: 'full' };
  const startRaw = spec.slice(0, dash);
  const endRaw = spec.slice(dash + 1);
  if (startRaw.length === 0) {
    const n = parseRangeDigits(endRaw);
    if (n === null) return { kind: 'full' };
    if (body === 0n || n === 0n) return { kind: 'unsatisfiable' };
    const take = n < body ? n : body;
    return { kind: 'partial', start: body - take, end: body - 1n };
  }
  const start = parseRangeDigits(startRaw);
  if (start === null) return { kind: 'full' };
  if (endRaw.length === 0) {
    if (start >= body) return { kind: 'unsatisfiable' };
    return { kind: 'partial', start, end: body - 1n };
  }
  const end = parseRangeDigits(endRaw);
  if (end === null) return { kind: 'full' };
  if (start > end) return { kind: 'full' };
  if (start >= body) return { kind: 'unsatisfiable' };
  return { kind: 'partial', start, end: end < body ? end : body - 1n };
}

/** `Content-Range` value for a 206, mirroring `content_range_value`. */
export function contentRangeValue(start: bigint, end: bigint, len: number): string {
  return `bytes ${start.toString()}-${end.toString()}/${len}`;
}

/** Content-addressed artifact key, mirroring `artifact_r2_key` (lowercase hex, slash layout). */
export function artifactR2Key(sha256hex: string): string | null {
  if (sha256hex.length !== 64 || !/^[0-9a-fA-F]{64}$/.test(sha256hex)) return null;
  const sha = sha256hex.toLowerCase();
  return `audio/${sha.slice(0, 2)}/${sha.slice(2, 4)}/${sha}`;
}

export interface MatrixArtifact {
  format: string;
  bitrate_kbps: number;
  sample_rate_hz: number;
  channels: number;
  sha256: string;
  r2_key: string;
}

export interface MatrixRequest {
  bearer: 'ok' | 'revoked' | 'malformed' | 'missing';
  scopes: string[];
  member: boolean;
  grant: 'ok' | 'expired' | 'tampered' | 'malformed' | 'over-ttl';
  grant_track_match: boolean;
  grant_profile: null | 'source' | 'opus-64';
  profile_param: null | string;
  artifact: null | MatrixArtifact;
  range: null | string;
  body_len: number;
  provider_auth: 'ok' | 'expired';
}

export interface MatrixVector {
  id: string;
  request: MatrixRequest;
  expect: {
    status: number;
    code: null | string;
    retryable: null | boolean;
    content_range?: string;
  };
}

const BEARER_STATES = new Set(['ok', 'revoked', 'malformed', 'missing']);
const GRANT_STATES = new Set(['ok', 'expired', 'tampered', 'malformed', 'over-ttl']);
const PROVIDER_STATES = new Set(['ok', 'expired']);

function decodeMatrixArtifact(id: string, raw: unknown): Check<null | MatrixArtifact> {
  if (raw === null) return { ok: true, value: null };
  if (!isRecord(raw)) return fail(`corpus-error: ${id} artifact is not an object or null`);
  const { format, bitrate_kbps: bitrate, sample_rate_hz: rate, channels, sha256, r2_key: key } = raw;
  if (
    typeof format !== 'string' ||
    typeof bitrate !== 'number' ||
    typeof rate !== 'number' ||
    typeof channels !== 'number' ||
    typeof sha256 !== 'string' ||
    typeof key !== 'string'
  ) {
    return fail(`corpus-error: ${id} artifact fields have the wrong types`);
  }
  return { ok: true, value: { format, bitrate_kbps: bitrate, sample_rate_hz: rate, channels, sha256, r2_key: key } };
}

function decodeMatrixVector(raw: unknown): Check<MatrixVector> {
  if (!isRecord(raw)) return fail('corpus-error: matrix row is not an object');
  const { id, request, expect } = raw;
  if (typeof id !== 'string' || id.length === 0) return fail('corpus-error: matrix row id');
  if (!isRecord(request)) return fail(`corpus-error: ${id} request is not an object`);
  const {
    bearer, scopes, member, grant, grant_track_match: trackMatch,
    grant_profile: grantProfile, profile_param: profileParam, artifact,
    range, body_len: bodyLen, provider_auth: providerAuth,
  } = request;
  if (typeof bearer !== 'string' || !BEARER_STATES.has(bearer)) {
    return fail(`corpus-error: ${id} bearer is not a known state`);
  }
  if (!Array.isArray(scopes) || !scopes.every((s): s is string => typeof s === 'string')) {
    return fail(`corpus-error: ${id} scopes is not a string array`);
  }
  if (typeof member !== 'boolean') return fail(`corpus-error: ${id} member is not a boolean`);
  if (typeof grant !== 'string' || !GRANT_STATES.has(grant)) {
    return fail(`corpus-error: ${id} grant is not a known state`);
  }
  if (typeof trackMatch !== 'boolean') return fail(`corpus-error: ${id} grant_track_match is not a boolean`);
  if (grantProfile !== null && grantProfile !== 'source' && grantProfile !== BOT_CONTRACT.profileName) {
    return fail(`corpus-error: ${id} grant_profile names an unknown profile`);
  }
  if (profileParam !== null && typeof profileParam !== 'string') {
    return fail(`corpus-error: ${id} profile_param is not a string or null`);
  }
  const artifactDecoded = decodeMatrixArtifact(id, artifact);
  if (!artifactDecoded.ok) return artifactDecoded;
  if (range !== null && typeof range !== 'string') return fail(`corpus-error: ${id} range is not a string or null`);
  if (typeof bodyLen !== 'number' || !Number.isSafeInteger(bodyLen) || bodyLen < 0) {
    return fail(`corpus-error: ${id} body_len is not a non-negative safe integer`);
  }
  if (typeof providerAuth !== 'string' || !PROVIDER_STATES.has(providerAuth)) {
    return fail(`corpus-error: ${id} provider_auth is not a known state`);
  }
  if (!isRecord(expect)) return fail(`corpus-error: ${id} expect is not an object`);
  const { status, code, retryable, content_range: contentRange } = expect;
  if (typeof status !== 'number' || !Number.isSafeInteger(status)) {
    return fail(`corpus-error: ${id} expect.status is not an integer`);
  }
  if (code !== null && typeof code !== 'string') return fail(`corpus-error: ${id} expect.code is not a string or null`);
  if (retryable !== null && typeof retryable !== 'boolean') {
    return fail(`corpus-error: ${id} expect.retryable is not a boolean or null`);
  }
  if (contentRange !== undefined && typeof contentRange !== 'string') {
    return fail(`corpus-error: ${id} expect.content_range is not a string`);
  }
  return {
    ok: true,
    value: {
      id,
      request: {
        bearer: bearer as MatrixRequest['bearer'],
        scopes,
        member,
        grant: grant as MatrixRequest['grant'],
        grant_track_match: trackMatch,
        grant_profile: grantProfile,
        profile_param: profileParam,
        artifact: artifactDecoded.value,
        range,
        body_len: bodyLen,
        provider_auth: providerAuth as MatrixRequest['provider_auth'],
      },
      expect: { status, code, retryable, ...(contentRange === undefined ? {} : { content_range: contentRange }) },
    },
  };
}

interface WireVerdict {
  status: number;
  code: null | string;
  retryable: null | boolean;
  contentRange: null | string;
  step: string;
}

function errorVerdict(code: string, step: string): WireVerdict {
  const spec = ERROR_REGISTRY[code];
  if (spec === undefined) throw new Error(`checker bug: unregistered code ${code}`);
  return { status: spec.status, code, retryable: spec.retryable, contentRange: null, step };
}

/**
 * Compute the wire verdict for one bot byte-fetch attempt under the banked
 * precedence (see the corpus README): bearer -> scope -> membership ->
 * grant -> track binding -> profile binding -> profile value -> artifact
 * presence -> range -> provider auth -> serve. Artifact descriptors are
 * server-side published rows, so a descriptor that contradicts the pinned
 * profile consts fails the row itself (`corpus-error`), whenever present.
 */
export function computeMatrixVerdict(request: MatrixRequest): Check<WireVerdict> {
  if (request.artifact !== null) {
    const artifact = request.artifact;
    if (
      artifact.format !== BOT_CONTRACT.artifactFormat ||
      artifact.bitrate_kbps !== BOT_CONTRACT.artifactBitrateKbps ||
      artifact.sample_rate_hz !== BOT_CONTRACT.artifactSampleRateHz ||
      artifact.channels !== BOT_CONTRACT.artifactChannels
    ) {
      return fail('corpus-error: artifact descriptor contradicts the pinned opus-64 consts');
    }
    const key = artifactR2Key(artifact.sha256);
    if (key === null || key !== artifact.r2_key) {
      return fail('corpus-error: artifact r2_key is not the content-addressed key of its sha256');
    }
  }
  if (request.bearer !== 'ok') return { ok: true, value: errorVerdict('unauthorized', 'bearer') };
  if (!request.scopes.includes(BOT_CONTRACT.streamReadScope)) {
    return { ok: true, value: errorVerdict('forbidden', 'scope') };
  }
  if (!request.member) return { ok: true, value: errorVerdict('not_found', 'membership') };
  if (request.grant !== 'ok') return { ok: true, value: errorVerdict('unauthorized', 'grant') };
  if (!request.grant_track_match) return { ok: true, value: errorVerdict('forbidden', 'track-binding') };
  const requested = request.profile_param ?? BOT_CONTRACT.sourceProfile;
  if (request.grant_profile !== null && request.grant_profile !== requested) {
    return { ok: true, value: errorVerdict('forbidden', 'profile-binding') };
  }
  if (request.profile_param !== null && request.profile_param !== BOT_CONTRACT.profileName) {
    return { ok: true, value: errorVerdict('invalid_request', 'profile-value') };
  }
  if (requested === BOT_CONTRACT.profileName && request.artifact === null) {
    return { ok: true, value: errorVerdict('not_found', 'artifact-absent') };
  }
  const range = resolveRange(request.range, request.body_len);
  if (range.kind === 'unsatisfiable') {
    return { ok: true, value: errorVerdict('range_not_satisfiable', 'range') };
  }
  if (request.provider_auth !== 'ok') {
    return { ok: true, value: errorVerdict('provider_auth_expired', 'provider-auth') };
  }
  if (range.kind === 'partial') {
    return {
      ok: true,
      value: {
        status: 206,
        code: null,
        retryable: null,
        contentRange: contentRangeValue(range.start, range.end, request.body_len),
        step: 'serve-partial',
      },
    };
  }
  return { ok: true, value: { status: 200, code: null, retryable: null, contentRange: null, step: 'serve-full' } };
}

/** Check one verdict-matrix row: the computed verdict must equal `expect`. */
export function checkMatrixRow(raw: unknown): Check<{ id: string }> {
  const decoded = decodeMatrixVector(raw);
  if (!decoded.ok) return decoded;
  const vec = decoded.value;
  const computed = computeMatrixVerdict(vec.request);
  if (!computed.ok) return fail(`${vec.id}: ${computed.reason}`);
  const verdict = computed.value;
  const expect = vec.expect;
  const mismatch =
    verdict.status !== expect.status ||
    verdict.code !== expect.code ||
    verdict.retryable !== expect.retryable ||
    verdict.contentRange !== (expect.content_range ?? null);
  if (mismatch) {
    const show = (v: WireVerdict): string =>
      `${v.status}/${v.code ?? 'no-code'}${v.contentRange === null ? '' : ` ${v.contentRange}`} (via ${v.step})`;
    return fail(
      `${vec.id}: expected ${expect.status}/${expect.code ?? 'no-code'}${expect.content_range === undefined ? '' : ` ${expect.content_range}`}, computed ${show(verdict)}`,
    );
  }
  return { ok: true, value: { id: vec.id } };
}

export interface CorpusFile {
  version: number;
  vectors: unknown[];
}

/**
 * Decode one corpus file: the `version` must match the checker, ids must be
 * unique non-empty strings. Returns the raw vectors for the typed checkers.
 */
export function decodeCorpusFile(raw: unknown, name: string): Check<unknown[]> {
  if (!isRecord(raw)) return fail(`corpus-error: ${name} is not an object`);
  if (raw.version !== BOT_CONTRACT.corpusVersion) {
    return fail(`corpus-error: ${name} version ${String(raw.version)} != checker ${BOT_CONTRACT.corpusVersion}`);
  }
  if (!Array.isArray(raw.vectors)) return fail(`corpus-error: ${name} vectors is not an array`);
  const seen = new Set<string>();
  for (const vec of raw.vectors) {
    if (!isRecord(vec) || typeof vec.id !== 'string' || vec.id.length === 0) {
      return fail(`corpus-error: ${name} has a vector without a string id`);
    }
    if (seen.has(vec.id)) return fail(`corpus-error: ${name} repeats id ${vec.id}`);
    seen.add(vec.id);
  }
  return { ok: true, value: raw.vectors };
}
