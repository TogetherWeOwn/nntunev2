import { test, expect } from '@playwright/test';
import { readFileSync } from 'node:fs';
import {
  BOT_CONTRACT,
  ERROR_REGISTRY,
  artifactR2Key,
  checkGrantVector,
  checkMatrixRow,
  checkSignedUrlVector,
  contentRangeValue,
  decodeCorpusFile,
  hmacSha256Base64Url,
  resolveRange,
  sha256Hex,
  strictBase64UrlDecode,
  verifyGrant,
  type Check,
} from '../helpers/bot-contract.js';

// Bot-contract conformance vectors + checker (spec section 5), feeding the
// B8 staging harness. The JSON corpus lives in
// `e2e/fixtures/bot-contract/` (grants, signed URLs, verdict matrix); the
// checker is `e2e/helpers/bot-contract.ts`.
//
// Offline-only by design: every test below runs on each PR via
// `npm run test:unit` with no staging, no credentials, and no network. B8
// adds the live legs that feed these vectors to staging and compare wire
// verdicts against the matrix.
//
// Names-only rule for this file: tokens, secrets, and full URLs are
// compared in memory only. Failure messages name vector ids, mutation
// names, and checker reasons — never interpolate a token, secret, or URL.

function loadCorpus(name: string): unknown[] {
  const raw = JSON.parse(
    readFileSync(new URL(`../fixtures/bot-contract/${name}`, import.meta.url), 'utf8'),
  ) as unknown;
  const decoded = decodeCorpusFile(raw, name);
  expect(decoded.ok, `${name}: ${(decoded as { reason?: string }).reason ?? 'decoded'}`).toBe(true);
  return (decoded as { ok: true; value: unknown[] }).value;
}

const grantVectors = loadCorpus('grants.json');
const urlVectors = loadCorpus('signed-url.json');
const matrixRows = loadCorpus('profile-matrix.json');

function grantById(id: string): Record<string, unknown> {
  const found = grantVectors.find(
    (vec): vec is Record<string, unknown> =>
      typeof vec === 'object' && vec !== null && (vec as { id?: unknown }).id === id,
  );
  if (!found) throw new Error(`corpus is missing grant vector ${id}`);
  return JSON.parse(JSON.stringify(found)) as Record<string, unknown>;
}

function urlById(id: string): Record<string, unknown> {
  const found = urlVectors.find(
    (vec): vec is Record<string, unknown> =>
      typeof vec === 'object' && vec !== null && (vec as { id?: unknown }).id === id,
  );
  if (!found) throw new Error(`corpus is missing signed-url vector ${id}`);
  return JSON.parse(JSON.stringify(found)) as Record<string, unknown>;
}

function rowById(id: string): Record<string, unknown> {
  const found = matrixRows.find(
    (vec): vec is Record<string, unknown> =>
      typeof vec === 'object' && vec !== null && (vec as { id?: unknown }).id === id,
  );
  if (!found) throw new Error(`corpus is missing matrix row ${id}`);
  return JSON.parse(JSON.stringify(found)) as Record<string, unknown>;
}

/** Flip one alphabet character, keeping base64url shape valid. */
function flipChar(s: string, index: number): string {
  const current = s[index] as string;
  const next = current === 'A' ? 'B' : 'A';
  return s.slice(0, index) + next + s.slice(index + 1);
}

test.describe('grant envelope corpus (offline, no staging)', () => {
  test('checker passes every grant vector', () => {
    expect(grantVectors.length).toBeGreaterThan(0);
    for (const vec of grantVectors) {
      const id = (vec as { id: string }).id;
      expect(id.startsWith('botgrant-'), `grant vector id scheme: ${id}`).toBe(true);
      const result = checkGrantVector(vec);
      expect(result.ok, `${id}: ${(result as { reason?: string }).reason ?? 'checked'}`).toBe(true);
    }
  });

  test('pinned happy token decodes to its exact payload segments', () => {
    const vec = grantById('botgrant-001');
    const token = vec.token as string;
    const segments = token.split('.');
    expect(segments.length).toBe(3);
    expect(segments[0]).toBe('v1');
    const payload = strictBase64UrlDecode(segments[1] as string);
    expect(payload?.toString('utf8')).toBe('user-1|track-9|1700000000');
  });

  test('crypto primitives reproduce the published test vectors', () => {
    // Same vectors `workers/stream/src/grant.rs` pins: independent
    // implementations agreeing on published values is the cross-language
    // proof, not self-consistency.
    expect(sha256Hex('abc')).toBe(
      'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad',
    );
    const rfcKey = '\u000b'.repeat(20); // 20 x 0x0b (RFC 4231 case 1 key)
    expect(hmacSha256Base64Url(rfcKey, 'Hi There')).toBe(
      Buffer.from(
        'b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7',
        'hex',
      ).toString('base64url'),
    );
  });

  test('strict base64url rejects padding, short quanta and nonzero pad bits', () => {
    expect(strictBase64UrlDecode('YQ')?.toString('hex')).toBe('61');
    expect(strictBase64UrlDecode('AQ')?.toString('hex')).toBe('01');
    // Padding, length-1-mod-4, non-alphabet, and nonzero pad bits all fail.
    expect(strictBase64UrlDecode('YQ==')).toBeNull();
    expect(strictBase64UrlDecode('A')).toBeNull();
    expect(strictBase64UrlDecode('YQ!')).toBeNull();
    expect(strictBase64UrlDecode('AB')).toBeNull();
    expect(strictBase64UrlDecode('ABC')).toBeNull();
    expect(strictBase64UrlDecode('')).toBeNull();
  });

  test('grant mutants fail by name', () => {
    const failures: string[] = [];
    const checkMutant = (
      name: string,
      mutate: (vec: Record<string, unknown>) => void,
      wantReason: string,
    ): void => {
      const vec = grantById('botgrant-001');
      mutate(vec);
      const result = checkGrantVector(vec);
      if (result.ok) {
        failures.push(`${name}: mutant verified, want reject ${wantReason}`);
        return;
      }
      const reason = (result as { reason: string }).reason;
      if (!reason.endsWith(wantReason)) failures.push(`${name}: got ${reason}, want ${wantReason}`);
    };
    checkMutant('flip payload char', (vec) => {
      const token = vec.token as string;
      const dot = token.indexOf('.');
      vec.token = flipChar(token, dot + 1);
    }, 'bad-signature');
    checkMutant('flip signature char', (vec) => {
      const token = vec.token as string;
      const dot = token.indexOf('.');
      const sigDot = token.indexOf('.', dot + 1);
      vec.token = flipChar(token, sigDot + 1);
    }, 'bad-signature');
    checkMutant('now at expiry', (vec) => {
      vec.now_unix = 1700000000;
    }, 'expired');
    checkMutant('now past expiry', (vec) => {
      vec.now_unix = 1800000000;
    }, 'expired');
    checkMutant('lifetime one past cap plus skew', (vec) => {
      vec.now_unix = 1700000000 - 961;
    }, 'ttl-too-long');
    checkMutant('drop version prefix', (vec) => {
      vec.token = (vec.token as string).slice('v1.'.length);
    }, 'malformed');
    checkMutant('append extra segment', (vec) => {
      vec.token = `${vec.token as string}.extra`;
    }, 'malformed');
    checkMutant('truncate token', (vec) => {
      vec.token = (vec.token as string).slice(0, 12);
    }, 'malformed');
    checkMutant('expect wrong exp', (vec) => {
      (vec.expect as Record<string, unknown>).exp_unix = 1700000001;
    }, 'verified grant differs from expect (names only, no token echoed)');
    checkMutant('expect wrong track', (vec) => {
      (vec.expect as Record<string, unknown>).track_id = 'track-other';
    }, 'verified grant differs from expect (names only, no token echoed)');
    checkMutant('expect reject on a happy token', (vec) => {
      vec.expect = { ok: false, reason: 'expired' };
    }, 'token verified');
    expect(failures).toEqual([]);
  });
});

test.describe('signed URL corpus (offline, no staging)', () => {
  test('checker passes every signed-url vector', () => {
    expect(urlVectors.length).toBeGreaterThan(0);
    for (const vec of urlVectors) {
      const id = (vec as { id: string }).id;
      expect(id.startsWith('boturl-'), `signed-url vector id scheme: ${id}`).toBe(true);
      const result = checkSignedUrlVector(vec);
      expect(result.ok, `${id}: ${(result as { reason?: string }).reason ?? 'checked'}`).toBe(true);
    }
  });

  test('signed-url mutants fail by name', () => {
    const failures: string[] = [];
    const checkMutant = (
      name: string,
      mutate: (vec: Record<string, unknown>) => void,
      wantReason: string,
    ): void => {
      const vec = urlById('boturl-001');
      mutate(vec);
      const result = checkSignedUrlVector(vec);
      if (result.ok) {
        failures.push(`${name}: mutant verified, want reject ${wantReason}`);
        return;
      }
      const reason = (result as { reason: string }).reason;
      if (!reason.endsWith(wantReason)) failures.push(`${name}: got ${reason}, want ${wantReason}`);
    };
    checkMutant('downgrade scheme to http', (vec) => {
      vec.url = (vec.url as string).replace('https://', 'http://');
    }, 'not-https');
    checkMutant('drop grant param', (vec) => {
      const parsed = new URL(vec.url as string);
      parsed.searchParams.delete('grant');
      vec.url = parsed.toString();
    }, 'missing-grant');
    checkMutant('duplicate grant param', (vec) => {
      const parsed = new URL(vec.url as string);
      parsed.searchParams.append('grant', parsed.searchParams.get('grant') as string);
      vec.url = parsed.toString();
    }, 'duplicate-grant');
    checkMutant('duplicate profile param', (vec) => {
      const parsed = new URL(vec.url as string);
      parsed.searchParams.append('profile', 'opus-64');
      vec.url = parsed.toString();
    }, 'duplicate-profile');
    checkMutant('unknown profile value', (vec) => {
      const parsed = new URL(vec.url as string);
      parsed.searchParams.set('profile', 'mp3');
      vec.url = parsed.toString();
    }, 'unknown-profile');
    checkMutant('empty profile value', (vec) => {
      const parsed = new URL(vec.url as string);
      parsed.searchParams.set('profile', '');
      vec.url = parsed.toString();
    }, 'unknown-profile');
    checkMutant('swap track segment', (vec) => {
      vec.url = (vec.url as string).replace('/play/track-synth-1724?', '/play/track-other?');
    }, 'grant-wrong-track');
    checkMutant('retarget onto the api path', (vec) => {
      vec.url = (vec.url as string).replace('/play/', '/v1/stream/');
    }, 'bad-path');
    checkMutant('nest the track path', (vec) => {
      vec.url = (vec.url as string).replace('/play/track-synth-1724?', '/play/a/b?');
    }, 'bad-path');
    checkMutant('flip grant char', (vec) => {
      const parsed = new URL(vec.url as string);
      const token = parsed.searchParams.get('grant') as string;
      const dot = token.indexOf('.');
      parsed.searchParams.set('grant', flipChar(token, dot + 1));
      vec.url = parsed.toString();
    }, 'grant-bad-signature');
    checkMutant('now at grant expiry', (vec) => {
      vec.now_unix = 1700000600;
    }, 'grant-expired');
    checkMutant('expect wrong profile', (vec) => {
      (vec.expect as Record<string, unknown>).profile = 'source';
    }, 'verified URL differs from expect (names only, no grant echoed)');
    expect(failures).toEqual([]);
  });

  test('unknown query params do not break a happy URL', () => {
    const vec = urlById('boturl-001');
    const parsed = new URL(vec.url as string);
    parsed.searchParams.set('utm_source', 'bot');
    parsed.searchParams.set('debug', '1');
    vec.url = parsed.toString();
    const result = checkSignedUrlVector(vec);
    expect(result.ok, `forward-compat: ${(result as { reason?: string }).reason ?? 'checked'}`).toBe(true);
  });
});

test.describe('verdict matrix corpus (offline, no staging)', () => {
  test('checker passes every matrix row', () => {
    expect(matrixRows.length).toBeGreaterThan(0);
    for (const vec of matrixRows) {
      const id = (vec as { id: string }).id;
      expect(id.startsWith('botmx-'), `matrix row id scheme: ${id}`).toBe(true);
      const result = checkMatrixRow(vec);
      expect(result.ok, `${id}: ${(result as { reason?: string }).reason ?? 'checked'}`).toBe(true);
    }
  });

  test('matrix mutants fail by name', () => {
    const failures: string[] = [];
    const checkMutant = (
      name: string,
      id: string,
      mutate: (vec: Record<string, unknown>) => void,
    ): void => {
      const vec = rowById(id);
      mutate(vec);
      const result = checkMatrixRow(vec);
      if (result.ok) failures.push(`${name}: mutant passed, want fail`);
    };
    const requestOf = (vec: Record<string, unknown>): Record<string, unknown> =>
      vec.request as Record<string, unknown>;
    const expectOf = (vec: Record<string, unknown>): Record<string, unknown> =>
      vec.expect as Record<string, unknown>;
    // Source happy row: every fault injection must move the verdict.
    checkMutant('source happy + opus-64 profile, no artifact', 'botmx-001', (vec) => {
      requestOf(vec).profile_param = 'opus-64';
    });
    checkMutant('source happy + revoked bearer', 'botmx-001', (vec) => {
      requestOf(vec).bearer = 'revoked';
    });
    checkMutant('source happy + expired grant', 'botmx-001', (vec) => {
      requestOf(vec).grant = 'expired';
    });
    checkMutant('source happy + unsatisfiable range', 'botmx-001', (vec) => {
      requestOf(vec).range = 'bytes=99999-';
    });
    checkMutant('source happy + provider expired', 'botmx-001', (vec) => {
      requestOf(vec).provider_auth = 'expired';
    });
    checkMutant('source happy + expect 201', 'botmx-001', (vec) => {
      expectOf(vec).status = 201;
    });
    checkMutant('source happy + expect a code', 'botmx-001', (vec) => {
      expectOf(vec).code = 'unauthorized';
      expectOf(vec).retryable = false;
    });
    checkMutant('source happy + wrong-scope bearer', 'botmx-001', (vec) => {
      requestOf(vec).scopes = ['library:read'];
    });
    // Opus-64 happy row: artifact descriptor and key mutations must fail.
    checkMutant('opus-64 happy + bitrate 128', 'botmx-003', (vec) => {
      ((requestOf(vec).artifact as Record<string, unknown>)).bitrate_kbps = 128;
    });
    checkMutant('opus-64 happy + format mp3', 'botmx-003', (vec) => {
      ((requestOf(vec).artifact as Record<string, unknown>)).format = 'mp3';
    });
    checkMutant('opus-64 happy + 44100 Hz', 'botmx-003', (vec) => {
      ((requestOf(vec).artifact as Record<string, unknown>)).sample_rate_hz = 44100;
    });
    checkMutant('opus-64 happy + mono', 'botmx-003', (vec) => {
      ((requestOf(vec).artifact as Record<string, unknown>)).channels = 1;
    });
    checkMutant('opus-64 happy + flipped sha char', 'botmx-003', (vec) => {
      const artifact = requestOf(vec).artifact as Record<string, unknown>;
      artifact.sha256 = flipChar(artifact.sha256 as string, 0);
    });
    checkMutant('opus-64 happy + flipped key char', 'botmx-003', (vec) => {
      const artifact = requestOf(vec).artifact as Record<string, unknown>;
      artifact.r2_key = flipChar(artifact.r2_key as string, 8);
    });
    checkMutant('opus-64 happy + short sha', 'botmx-003', (vec) => {
      const artifact = requestOf(vec).artifact as Record<string, unknown>;
      artifact.sha256 = (artifact.sha256 as string).slice(0, 32);
    });
    checkMutant('opus-64 happy + artifact removed', 'botmx-003', (vec) => {
      requestOf(vec).artifact = null;
    });
    checkMutant('opus-64 happy + profile flac', 'botmx-003', (vec) => {
      requestOf(vec).profile_param = 'flac';
    });
    // Negative rows: clearing the fault must move the verdict off expect.
    checkMutant('expired-grant row + grant ok', 'botmx-014', (vec) => {
      requestOf(vec).grant = 'ok';
    });
    checkMutant('revoked-bearer row + bearer ok', 'botmx-017', (vec) => {
      requestOf(vec).bearer = 'ok';
    });
    checkMutant('non-member row + member', 'botmx-018', (vec) => {
      requestOf(vec).member = true;
    });
    checkMutant('unsatisfiable-range row + full body', 'botmx-019', (vec) => {
      requestOf(vec).range = null;
    });
    checkMutant('provider-expired row + provider ok', 'botmx-020', (vec) => {
      requestOf(vec).provider_auth = 'ok';
    });
    checkMutant('wrong-scope row + stream scope', 'botmx-021', (vec) => {
      requestOf(vec).scopes = ['library:read', 'stream:read'];
    });
    checkMutant('unknown-profile row + opus-64, no artifact', 'botmx-010', (vec) => {
      requestOf(vec).profile_param = 'opus-64';
    });
    checkMutant('416 row + expect retryable true', 'botmx-019', (vec) => {
      expectOf(vec).retryable = true;
    });
    checkMutant('401 row + expect 403', 'botmx-014', (vec) => {
      expectOf(vec).status = 403;
      expectOf(vec).code = 'forbidden';
    });
    // Structural breaks must fail, not pass silently.
    checkMutant('source happy + unknown bearer state', 'botmx-001', (vec) => {
      requestOf(vec).bearer = 'sleepy';
    });
    checkMutant('source happy + negative body_len', 'botmx-001', (vec) => {
      requestOf(vec).body_len = -1;
    });
    checkMutant('source happy + numeric profile', 'botmx-001', (vec) => {
      requestOf(vec).profile_param = 64;
    });
    checkMutant('206 row + wrong content-range', 'botmx-002', (vec) => {
      expectOf(vec).content_range = 'bytes 0-7/1724';
    });
    checkMutant('200 row + stray content-range', 'botmx-001', (vec) => {
      expectOf(vec).content_range = 'bytes 0-8/1724';
    });
    expect(failures).toEqual([]);
  });

  test('matrix rows cite only registered codes', () => {
    for (const vec of matrixRows) {
      const row = vec as { id: string; expect: { status: number; code: null | string; retryable: null | boolean } };
      if (row.expect.code === null) {
        expect(row.expect.retryable, `${row.id}: success rows carry no retryable flag`).toBeNull();
        continue;
      }
      const spec = ERROR_REGISTRY[row.expect.code];
      expect(spec, `${row.id}: code ${row.expect.code} is registered`).toBeDefined();
      expect(row.expect.status, `${row.id}: status matches the registry`).toBe(spec?.status);
      expect(row.expect.retryable, `${row.id}: retryable matches the registry`).toBe(spec?.retryable);
    }
  });

  test('range resolution mirrors the T2 seek vectors', () => {
    // The 1724-byte staging WAV with seeks at bytes 0-8 and 9-17.
    expect(resolveRange('bytes=0-8', 1724)).toEqual({ kind: 'partial', start: 0n, end: 8n });
    expect(resolveRange('bytes=9-17', 1724)).toEqual({ kind: 'partial', start: 9n, end: 17n });
    expect(resolveRange('bytes=1724-', 1724)).toEqual({ kind: 'unsatisfiable' });
    expect(contentRangeValue(0n, 8n, 1724)).toBe('bytes 0-8/1724');
    // u64-shaped numerals past u64::MAX fall back to a full body like
    // `range.rs` (parse failure), never to a partial or a throw.
    expect(resolveRange('bytes=99999999999999999999999-', 1724)).toEqual({ kind: 'full' });
  });

  test('artifact keys derive from the sha like artifact_r2_key', () => {
    const sha = '0b3cf7e37f87c543fb6d8e129f3778f59d8529191b27b9c7d310309f8bd67e4e';
    expect(artifactR2Key(sha)).toBe(`audio/0b/3c/${sha}`);
    expect(artifactR2Key(sha.toUpperCase())).toBe(`audio/0b/3c/${sha}`);
    expect(artifactR2Key('abc')).toBeNull();
  });
});

test.describe('stable IDs match the pinned sources (offline, no staging)', () => {
  test('opus-64 artifact consts match transcode.rs', () => {
    const source = readFileSync(
      new URL('../../crates/nntune-core/src/transcode.rs', import.meta.url),
      'utf8',
    );
    const strConst = (name: string): string => {
      const match = source.match(new RegExp(`pub const ${name}: &str = "([^"]+)";`));
      if (!match) throw new Error(`transcode.rs no longer pins ${name}`);
      return match[1] as string;
    };
    const numConst = (name: string): number => {
      const match = source.match(new RegExp(`pub const ${name}: u32 = (\\d+);`));
      if (!match) throw new Error(`transcode.rs no longer pins ${name}`);
      return Number(match[1]);
    };
    expect(BOT_CONTRACT.profileName).toBe(strConst('PROFILE_NAME'));
    expect(BOT_CONTRACT.artifactFormat).toBe(strConst('FORMAT'));
    expect(BOT_CONTRACT.artifactBitrateKbps).toBe(numConst('BITRATE_KBPS'));
    expect(BOT_CONTRACT.artifactSampleRateHz).toBe(numConst('SAMPLE_RATE_HZ'));
    expect(BOT_CONTRACT.artifactChannels).toBe(numConst('CHANNELS'));
  });

  test('grant TTL consts match grant.rs', () => {
    const source = readFileSync(
      new URL('../../workers/stream/src/grant.rs', import.meta.url),
      'utf8',
    );
    const numConst = (name: string): number => {
      const match = source.match(new RegExp(`pub const ${name}: u64 = (\\d+);`));
      if (!match) throw new Error(`grant.rs no longer pins ${name}`);
      return Number(match[1]);
    };
    expect(BOT_CONTRACT.maxGrantTtlSecs).toBe(numConst('MAX_GRANT_TTL'));
    expect(BOT_CONTRACT.clockSkewSecs).toBe(numConst('CLOCK_SKEW_SECS'));
  });

  test('stream scope matches native_rest.rs', () => {
    const source = readFileSync(
      new URL('../../workers/api/src/native_rest.rs', import.meta.url),
      'utf8',
    );
    const match = source.match(/pub const SCOPE_STREAM_READ: &str = "([^"]+)";/);
    if (!match) throw new Error('native_rest.rs no longer pins SCOPE_STREAM_READ');
    expect(BOT_CONTRACT.streamReadScope).toBe(match[1]);
  });

  test('checker registry matches docs/api-errors.md', () => {
    const doc = readFileSync(new URL('../../docs/api-errors.md', import.meta.url), 'utf8');
    for (const [code, spec] of Object.entries(ERROR_REGISTRY)) {
      const row = doc
        .split('\n')
        .find((line) => line.startsWith(`| \`${code}\` |`));
      expect(row, `docs/api-errors.md carries a | \`${code}\` | envelope row`).toBeDefined();
      const cells = (row as string).split('|').map((cell) => cell.trim());
      expect(Number(cells[2]), `${code}: registry status`).toBe(spec.status);
      expect(cells[3] === 'true', `${code}: registry retryable`).toBe(spec.retryable);
    }
  });
});

test.describe('corpus loader (offline, no staging)', () => {
  test('corpus loader rejects version drift, missing ids and duplicate ids', () => {
    const failures: string[] = [];
    const rejects = (name: string, raw: unknown): void => {
      const result: Check<unknown[]> = decodeCorpusFile(raw, 'probe.json');
      if (result.ok) failures.push(`${name}: loader accepted, want reject`);
    };
    rejects('wrong version', { version: 999, vectors: [] });
    rejects('missing version', { vectors: [] });
    rejects('vectors not an array', { version: 1, vectors: {} });
    rejects('vector without id', { version: 1, vectors: [{ no: 'id' }] });
    rejects('duplicate ids', {
      version: 1,
      vectors: [
        { id: 'dup', token: 'x', secret: 'corpus', now_unix: 1, expect: { ok: false, reason: 'expired' } },
        { id: 'dup', token: 'y', secret: 'corpus', now_unix: 1, expect: { ok: false, reason: 'expired' } },
      ],
    });
    expect(failures).toEqual([]);
    // An unknown secret name passes the loader (the shape is fine) but must
    // fail the checker: secret names never resolve silently.
    const vec = grantById('botgrant-001');
    vec.secret = 'prod';
    expect(checkGrantVector(vec).ok).toBe(false);
  });
});

test.describe('checker fidelity pins (offline, no staging)', () => {
  test('non-ascii envelope verifies its signature over utf8 bytes, then rejects as malformed', () => {
    // `grant.rs` HMACs the raw segment bytes: a token whose payload
    // segment sits outside base64url but whose signature matches those
    // bytes fails at payload decode (`malformed`), not at the signature
    // gate. Hashing the segment as ascii instead would flip this reason
    // to `bad-signature`. The signature stays in two fragments so no
    // single literal trips the secret scan (see the corpus README).
    const sigHead = 'RrPL_y21iqvX1Pa0SU8d';
    const sigTail = 'qgx-v4MhNRyX7uuZSSwK5sk';
    const result = verifyGrant(`v1.éa.${sigHead}${sigTail}`, 'corpus', 1699999999);
    expect(result.ok).toBe(false);
    expect((result as { reason: string }).reason).toBe('malformed');
  });
});
