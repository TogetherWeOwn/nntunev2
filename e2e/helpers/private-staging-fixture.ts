import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';

const DIGEST = /^[a-f0-9]{64}$/;
const TRACK = /^[a-zA-Z0-9_-]{1,128}$/;
const AUDIO = new URL('../../fixtures/synthetic-flac/private-staging/', import.meta.url);

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
    return { sha256, body };
  });
}

export interface PrivateStagingFixture {
  email: string;
  inviteCode: string;
  expiresAt: number;
  publications: {
    sha256: string;
    trackId: string;
  }[];
}

export function parsePrivateStagingFixture(raw: string, now: number): PrivateStagingFixture {
  const invalid = () => new Error('Private staging requires a fresh synthetic invite and two verified publication records');
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
    !Array.isArray(fixture.publications) || fixture.publications.length !== 2 ||
    fixture.publications.some((entry) =>
      !entry || typeof entry.trackId !== 'string' || !TRACK.test(entry.trackId) ||
      typeof entry.sha256 !== 'string' || !DIGEST.test(entry.sha256)
    ) || new Set(fixture.publications.map((entry) => entry.trackId)).size !== 2 ||
    new Set(fixture.publications.map((entry) => entry.sha256)).size !== 2
  ) throw invalid();
  return {
    email: fixture.email, inviteCode: fixture.inviteCode, expiresAt: fixture.expiresAt,
    publications: fixture.publications,
  };
}
