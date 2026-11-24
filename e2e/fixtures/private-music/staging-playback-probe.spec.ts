import { expect, test, type APIRequestContext, type APIResponse } from '@playwright/test';
import { privateStagingAudio } from '../../helpers/private-staging-fixture.js';
import { assertStagingPlaybackProbe } from '../../helpers/staging-playback-probe.js';

const origin = 'https://staging.nntune.com';
const sourceUrl = `${origin}/v1/plays/media/fixture-track/fixture-tab/private_browser_${'a'.repeat(22)}`;
const [fixture, other] = privateStagingAudio();
const options = { sourceUrl, trackId: 'fixture-track', fixtureSha256: fixture!.sha256 };

const full = {
  status: 200,
  headers: {
    'content-type': 'audio/flac',
    'accept-ranges': 'bytes',
    'content-length': String(fixture!.body.length),
  } as Record<string, string>,
  body: fixture!.body,
};
const seek = {
  status: 206,
  headers: { ...full.headers, 'content-range': `bytes 65536-69631/${full.body.length}`, 'content-length': '4096' },
  body: full.body.subarray(65536, 69632),
};

function transport(...responses: (typeof full)[]) {
  const calls: { url: string; options: Parameters<APIRequestContext['get']>[1] }[] = [];
  let disposed = 0;
  const request = {
    async get(url: string, options: Parameters<APIRequestContext['get']>[1]) {
      calls.push({ url, options });
      const result = responses.shift();
      if (!result) throw new Error('Unexpected fixture request');
      return {
        status: () => result.status,
        headers: () => result.headers,
        body: async () => result.body,
        dispose: async () => { disposed += 1; },
      } as APIResponse;
    },
  } as APIRequestContext;
  return { calls, request, disposed: () => disposed };
}

const env = {
  STAGING_BASE_URL: origin,
  CF_ACCESS_CLIENT_ID: 'synthetic-access-id',
  CF_ACCESS_CLIENT_SECRET: 'synthetic-access-secret',
};
const original = Object.entries(env).map(([name]) => [name, process.env[name]] as const);
test.beforeEach(() => { Object.assign(process.env, env); });
test.afterEach(() => {
  for (const [name, value] of original) {
    if (value === undefined) delete process.env[name];
    else process.env[name] = value;
  }
});

test('protected playback and a nonzero seek return the exact published FLAC bytes', async () => {
  const { request, calls, disposed } = transport(full, seek);
  await assertStagingPlaybackProbe({ request, ...options });
  const headers = {
    'CF-Access-Client-Id': env.CF_ACCESS_CLIENT_ID,
    'CF-Access-Client-Secret': env.CF_ACCESS_CLIENT_SECRET,
    'Sec-Fetch-Site': 'same-origin',
  };
  expect(calls).toEqual([
    { url: sourceUrl, options: { headers, maxRedirects: 0, timeout: 30_000 } },
    { url: sourceUrl, options: { headers: { ...headers, Range: 'bytes=65536-69631' }, maxRedirects: 0, timeout: 30_000 } },
  ]);
  expect(disposed()).toBe(2);
});

test('probe refuses mismatched tracks, non-staging and noncanonical sources before a request', async () => {
  const { request, calls } = transport();
  for (const candidate of [
    '', `${origin}/api/demo/audio`, `https://nntune.com${new URL(sourceUrl).pathname}`,
    sourceUrl.replace(origin, `${origin}.example.test`),
    sourceUrl.replace('staging.nntune.com', 'user@staging.nntune.com'),
    sourceUrl.replace('fixture-track', 'other-track'),
    sourceUrl.replace('fixture-tab', '%66ixture-tab'), sourceUrl.replace('private_browser_', 'private_'),
    `${sourceUrl}?opaque=value`, `${sourceUrl}#opaque`, `${sourceUrl}\n`,
  ]) {
    await expect(assertStagingPlaybackProbe({ request, ...options, sourceUrl: candidate })).rejects.toThrow(
      'requires the selected track’s staging media relay',
    );
  }
  await expect(assertStagingPlaybackProbe({ request, ...options, fixtureSha256: '0'.repeat(64) })).rejects.toThrow(
    'requires a published synthetic FLAC fixture',
  );
  process.env.STAGING_BASE_URL = 'https://nntune.com';
  await expect(assertStagingPlaybackProbe({ request, ...options })).rejects.toThrow('requires the staging origin');
  expect(calls).toHaveLength(0);
});

test('a redirect or Access denial cannot count as a positive playback response', async () => {
  for (const reply of [
    { ...full, status: 302, headers: { location: 'https://other.example.test/?sensitive-marker' } },
    { ...full, status: 403, headers: { 'cf-access-denied': 'sensitive-marker' } },
    { ...full, status: 401 },
  ]) {
    const { request, calls } = transport(reply);
    await expect(assertStagingPlaybackProbe({ request, ...options })).rejects.toThrow(
      /^(?!.*sensitive-marker).*?(unexpected redirect|Cloudflare Access wall|unexpected media status)/,
    );
    expect(calls).toHaveLength(1);
    expect(calls[0]?.options?.maxRedirects).toBe(0);
  }
});

test('full response requires the original bytes, media type and exact full-response headers', async () => {
  for (const reply of [
    { ...full, headers: { ...full.headers, 'content-type': 'audio/wav' } },
    { ...full, headers: { ...full.headers, 'content-length': '0' } },
    { ...full, headers: { ...full.headers, 'accept-ranges': 'none' } },
    { ...full, headers: { ...full.headers, 'content-range': seek.headers['content-range'] } },
    { ...full, body: Buffer.concat([other!.body, full.body]).subarray(0, full.body.length) },
  ]) {
    const { request, calls } = transport(reply);
    await expect(assertStagingPlaybackProbe({ request, ...options })).rejects.toThrow(
      /\[staging-playback-full\] media (headers|bytes) do not match/,
    );
    expect(calls).toHaveLength(1);
  }
});

test('seek proof rejects a full fallback, a shifted range and different bytes at the same length', async () => {
  for (const reply of [
    full,
    { ...seek, headers: { ...seek.headers, 'content-range': `bytes 65537-69632/${full.body.length}` } },
    { ...seek, body: full.body.subarray(0, 4096) },
    { ...seek, body: seek.body.subarray(1) },
  ]) {
    const { request, calls } = transport(full, reply);
    await expect(assertStagingPlaybackProbe({ request, ...options })).rejects.toThrow(/\[staging-playback-seek\]/);
    expect(calls).toHaveLength(2);
  }
});
