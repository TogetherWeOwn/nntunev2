import type { APIRequestContext } from '@playwright/test';
import { accessHeaders, assertNoAccessWall, stagingBaseUrl } from './access.js';
import { apiGet } from './accounts.js';
import { privateStagingAudio } from './private-staging-fixture.js';

const STAGING_ORIGIN = 'https://staging.nntune.com';
const MEDIA_PATH = /^\/v1\/plays\/media\/([A-Za-z0-9_-]{1,512})\/[A-Za-z0-9_-]{1,128}\/private_browser_[A-Za-z0-9_-]{22}$/;
const SEEK_START = 65_536;
const SEEK_END = 69_631;

/** Probe the active synthetic track through the browser's authorized media relay. */
export async function assertStagingPlaybackProbe({
  request, sourceUrl, trackId, fixtureSha256,
}: {
  request: APIRequestContext; // Use the authenticated invitee's page.request.
  sourceUrl: string; // The audio element's currentSrc, captured after Play.
  trackId: string;
  fixtureSha256: string;
}): Promise<void> {
  const path = sourceUrl.slice(STAGING_ORIGIN.length);
  const match = MEDIA_PATH.exec(path);
  if (!sourceUrl.startsWith(STAGING_ORIGIN) || !match || match[0] !== path || match[1] !== trackId) {
    throw new Error('Staging playback probe requires the selected track’s staging media relay');
  }
  const fixture = privateStagingAudio().find(({ sha256 }) => sha256 === fixtureSha256);
  if (!fixture || fixture.body.length <= SEEK_END) {
    throw new Error('Staging playback probe requires a published synthetic FLAC fixture');
  }
  if (stagingBaseUrl('fail-closed', 'staging-playback-probe') !== STAGING_ORIGIN) {
    throw new Error('Staging playback probe requires the staging origin');
  }

  const headers = { ...accessHeaders(), 'Sec-Fetch-Site': 'same-origin' };
  const assertMedia = async (
    phase: 'full' | 'seek',
    expected: Buffer,
    range?: { request: string; response: string },
  ) => {
    const label = `staging-playback-${phase}`;
    const response = await apiGet(request, sourceUrl, range ? { ...headers, Range: range.request } : headers, label);
    try {
      const responseHeaders = response.headers();
      const status = response.status();
      assertNoAccessWall({ status, headers: responseHeaders, bodyText: '', url: sourceUrl }, label);
      if (status !== (range ? 206 : 200)) {
        throw new Error(`[${label}] unexpected media status ${status}`);
      }
      if (
        responseHeaders['content-type'] !== 'audio/flac' ||
        responseHeaders['accept-ranges'] !== 'bytes' ||
        responseHeaders['content-length'] !== String(expected.length) ||
        responseHeaders['content-range'] !== range?.response
      ) {
        throw new Error(`[${label}] media headers do not match the FLAC fixture response`);
      }
      const bytes = await response.body().catch(() => {
        throw new Error(`[${label}] could not read the FLAC response`);
      });
      if (!bytes.equals(expected)) {
        throw new Error(`[${label}] media bytes do not match the FLAC fixture`);
      }
    } finally {
      await response.dispose().catch(() => {});
    }
  };

  await assertMedia('full', fixture.body);
  await assertMedia('seek', fixture.body.subarray(SEEK_START, SEEK_END + 1), {
    request: `bytes=${SEEK_START}-${SEEK_END}`,
    response: `bytes ${SEEK_START}-${SEEK_END}/${fixture.body.length}`,
  });
}
