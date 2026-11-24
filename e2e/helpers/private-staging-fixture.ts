import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';

const DIGEST = /^[a-f0-9]{64}$/;
const TRACK = /^[a-f0-9]{8}-(?:[a-f0-9]{4}-){3}[a-f0-9]{12}$/;
const AUDIO = new URL('../../fixtures/synthetic-flac/private-staging/', import.meta.url);
export const PRIVATE_TRIAL_UPLOAD = '/v1/admin/private-trial/upload';
export const SESSION_COOKIE = '__Host-nntune-session';

export function privateStagingAudio() {
  const manifest = JSON.parse(readFileSync(new URL('manifest.json', AUDIO), 'utf8')) as {
    tracks: { file: string; bytes: number; sha256: string }[];
  };
  return manifest.tracks.map(({ file, bytes, sha256 }) => {
    if (!/^tone-[ab]\.flac$/.test(file)) throw new Error('Unexpected private staging audio fixture filename');
    const body = readFileSync(new URL(file, AUDIO));
    if (body.length !== bytes || createHash('sha256').update(body).digest('hex') !== sha256) {
      throw new Error('Private staging audio does not match the checked-in manifest');
    }
    return { file, sha256, body };
  });
}

export interface PrivateStagingFixture {
  email: string;
  inviteCode: string;
  expiresAt: number;
  ownerSession: string;
  companion: { trackId: string; sha256: string };
}

export function parsePrivateStagingFixture(raw: string, now: number, companionDigest: string): PrivateStagingFixture {
  const invalid = () => new Error('Private staging requires a fresh synthetic invite, owner session and verified companion publication');
  if (!raw || raw.length > 4096) throw invalid();
  let fixture: Partial<PrivateStagingFixture> | null;
  try {
    fixture = JSON.parse(raw) as Partial<PrivateStagingFixture> | null;
  } catch {
    throw invalid();
  }
  if (
    typeof fixture?.email !== 'string' || !/^[a-z0-9._+-]{1,64}@example\.test$/.test(fixture.email) ||
    typeof fixture.inviteCode !== 'string' || !/^[A-Za-z0-9_-]{16,128}$/.test(fixture.inviteCode) ||
    typeof fixture.expiresAt !== 'number' || !Number.isSafeInteger(fixture.expiresAt) ||
    fixture.expiresAt <= now || fixture.expiresAt > now + 8 * 60 ||
    typeof fixture.ownerSession !== 'string' || !/^[A-Za-z0-9_-]{1,2048}\.[A-Za-z0-9_-]{43}$/.test(fixture.ownerSession) ||
    typeof fixture.companion?.trackId !== 'string' || !TRACK.test(fixture.companion.trackId) ||
    !DIGEST.test(companionDigest) || fixture.companion.sha256 !== companionDigest
  ) throw invalid();
  return {
    email: fixture.email, inviteCode: fixture.inviteCode, expiresAt: fixture.expiresAt,
    ownerSession: fixture.ownerSession,
    companion: { trackId: fixture.companion.trackId, sha256: fixture.companion.sha256 },
  };
}

export function freshTrialPublication(raw: string, expectedDigest: string, priorIds: readonly string[]) {
  const invalid = () => new Error('Expected a fresh canonical publication for the uploaded FLAC');
  let receipt: { trackId?: unknown; sha256?: unknown } | null;
  try {
    receipt = JSON.parse(raw);
  } catch {
    throw invalid();
  }
  if (typeof receipt?.trackId !== 'string' || !TRACK.test(receipt.trackId) ||
      Object.keys(receipt).length !== 2 || !DIGEST.test(expectedDigest) ||
      receipt.sha256 !== expectedDigest || priorIds.includes(receipt.trackId)) throw invalid();
  return { trackId: receipt.trackId, sha256: expectedDigest };
}
