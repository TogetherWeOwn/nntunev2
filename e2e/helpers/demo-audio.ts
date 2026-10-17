/**
 * Vendored synthetic demo-audio contract.
 *
 * Source of truth: `apps/web/src/lib/player/demoMedia.ts` (`syntheticBody`,
 * `MEDIA_BYTES`, `TRACK_SALT`). This copy is intentionally vendored — not
 * imported — so the staging spec proves the deployed app still serves the
 * pinned generator output instead of sharing its implementation. If the app
 * changes the generator (or the range parser, both pinned by TOG-28373),
 * update both files together; a mismatch here fails the live spec by name.
 *
 * No staging, no credentials, no Playwright dependency: pure functions pinned
 * by offline tests in `tests/demo-audio-contract.spec.ts`.
 */

import { Buffer } from 'node:buffer';

/** Full-body length of the deterministic WAV (`MEDIA_BYTES = 256 * 1024`). */
export const DEMO_MEDIA_BYTES = 256 * 1024;

const WAV_HEADER_BYTES = 44;
const SAMPLE_RATE = 8_000;

/**
 * Allowlisted track ids -> sample salt. Unknown ids serve the default
 * stream (salt 0). Mirrors `TRACK_SALT` in demoMedia.ts.
 */
export const DEMO_TRACK_SALTS: Readonly<Record<string, number>> = {
  'second-signal': 41,
  'third-light': 97,
};

/** Salt for a `?track=` value; unknown (or absent) tracks use the default 0. */
export function demoTrackSalt(track: string | null): number {
  if (track === null || !Object.hasOwn(DEMO_TRACK_SALTS, track)) return 0;
  return DEMO_TRACK_SALTS[track] ?? 0;
}

/**
 * The exact bytes the app must serve for a track: 44-byte PCM WAV header +
 * LE int16 samples `((sample * 7919 + 13 + salt) % 65536) - 32768`.
 */
export function expectedSyntheticBody(track: string | null = null): Buffer {
  const salt = demoTrackSalt(track);
  const body = Buffer.alloc(DEMO_MEDIA_BYTES);
  const dataBytes = DEMO_MEDIA_BYTES - WAV_HEADER_BYTES;

  body.write('RIFF', 0);
  body.writeUInt32LE(DEMO_MEDIA_BYTES - 8, 4);
  body.write('WAVEfmt ', 8);
  body.writeUInt32LE(16, 16);
  body.writeUInt16LE(1, 20);
  body.writeUInt16LE(1, 22);
  body.writeUInt32LE(SAMPLE_RATE, 24);
  body.writeUInt32LE(SAMPLE_RATE * 2, 28);
  body.writeUInt16LE(2, 32);
  body.writeUInt16LE(16, 34);
  body.write('data', 36);
  body.writeUInt32LE(dataBytes, 40);

  for (let offset = WAV_HEADER_BYTES, sample = 0; offset < DEMO_MEDIA_BYTES; offset += 2, sample += 1) {
    const value = ((sample * 7919 + 13 + salt) % 65_536) - 32_768;
    body.writeInt16LE(value, offset);
  }
  return body;
}

/**
 * Offset of the first differing byte, or null when the buffers are
 * byte-identical. Reported in failure messages so a drift names where the
 * served stream diverges from the vendored contract.
 */
export function firstByteDifference(actual: Buffer, expected: Buffer): number | null {
  const common = Math.min(actual.length, expected.length);
  for (let i = 0; i < common; i += 1) {
    if (actual[i] !== expected[i]) return i;
  }
  return actual.length === expected.length ? null : common;
}
