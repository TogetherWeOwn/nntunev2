import { test, expect } from '@playwright/test';
import { connect } from 'node:tls';
import { syntheticNntpEndpoint } from '../helpers/access.js';
import {
  PROVIDER_BUDGET_ENV,
  isNntpCode,
  mintProbeMessageId,
  readProviderBudget,
} from '../helpers/provider-budget.js';

// Provider 430 control: one direct NNTP connection to the real staging
// provider asserting a never-posted article answers 430 (no such article).
// This is the journey's live provider proof that costs almost nothing: no
// NZB is fetched, only a greeting + AUTHINFO + one STAT + QUIT cross the
// wire (~1 connection, ~1 KB).
//
// Owner rules, enforced by construction:
// - Budget: the run-wide cap comes from E2E_NNTP_MAX_CONNECTIONS (default 2,
//   hard max 10 — see helpers/provider-budget.ts). This leg opens exactly one
//   connection and never exceeds it.
// - 502 fail-fast: a 502 (too many connections) at any point throws a clear
//   error immediately. No retry loops that could hold connections — including
//   Playwright's own CI retry, disabled for this file below. The socket is
//   closed in a `finally` on every path.
// - Minimal traffic: the probe message-ID is minted fresh per run under the
//   reserved `.invalid` TLD (never a real posting, never NZB content) and the
//   leg fetches only the STAT reply.
// - Evidence: a counts-only JSON attachment (peak connections, bytes read,
//   verdict) — no IDs, no subjects, no content. Peak + bytes are also reported
//   in the PR evidence comment.
//
// File-level skip: pre-deploy (no STAGING_BASE_URL) the whole file skips
// without touching the network. See health.spec.ts.
test.skip(
  !process.env.STAGING_BASE_URL,
  'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.',
);

// No Playwright retries for this file: a retried control would open a second
// provider connection after a failure, which is exactly the retry loop the
// 502 fail-fast rule forbids. One attempt, then red.
test.describe.configure({ retries: 0 });

const STEP_TIMEOUT_MS = 20_000;

/** Read one CRLF-terminated NNTP reply line, counting bytes, with a deadline. */
function readReplyLine(
  socket: import('node:net').Socket,
  buffer: { text: string; bytes: number },
  context: string,
): Promise<string> {
  return new Promise((resolve, reject) => {
    const deadline = setTimeout(() => {
      cleanup();
      reject(new Error(`[${context}] timed out waiting for provider reply (names only).`));
    }, STEP_TIMEOUT_MS);
    const onData = (chunk: Buffer): void => {
      buffer.bytes += chunk.length;
      buffer.text += chunk.toString('utf8');
      const end = buffer.text.indexOf('\r\n');
      if (end !== -1) {
        const line = buffer.text.slice(0, end);
        buffer.text = buffer.text.slice(end + 2);
        cleanup();
        resolve(line);
      }
    };
    const onError = (error: Error): void => {
      cleanup();
      reject(new Error(`[${context}] provider socket error (names only): ${error.name}`));
    };
    const cleanup = (): void => {
      clearTimeout(deadline);
      socket.off('data', onData);
      socket.off('error', onError);
    };
    socket.on('data', onData);
    socket.on('error', onError);
  });
}

test('provider answers 430 for a never-posted article on one connection', async ({}, testInfo) => {
  const budget = readProviderBudget(process.env);
  expect(
    1 <= budget.maxConnections,
    `[provider-430] budget misconfigured (${PROVIDER_BUDGET_ENV} resolves below 1) — refusing to probe.`,
  ).toBe(true);

  const endpoint = syntheticNntpEndpoint();
  const password = process.env.SYNTHETIC_NNTP_PASS ?? '';
  if (!password) {
    throw new Error(
      '[provider-430] SYNTHETIC_NNTP_PASS is unset: the control needs the provider ' +
        'password from the environment (secret names only — never values).',
    );
  }
  // The probe ID is minted, never posted, never NZB content: only counts
  // (never its value) enter evidence.
  const probeId = mintProbeMessageId();

  let peakConnections = 0;
  const buffer = { text: '', bytes: 0 };
  const socket = connect({
    host: endpoint.host,
    port: endpoint.port,
    servername: endpoint.host,
  });
  peakConnections = 1;
  // Fail fast on connect: no retries, no reconnects — an unreachable provider
  // is a red control, not a wait loop. The deadline timer is cleared on both
  // outcomes (and listeners removed on timeout) so a fast success does not
  // leave a 20 s timer holding the Playwright worker open after the socket
  // is destroyed.
  const connected = new Promise<void>((resolve, reject) => {
    const timer = setTimeout(() => {
      socket.off('secureConnect', onSecureConnect);
      socket.off('error', onSocketError);
      socket.destroy();
      reject(new Error('[provider-430] provider connect timed out.'));
    }, STEP_TIMEOUT_MS);
    const onSecureConnect = (): void => {
      clearTimeout(timer);
      socket.off('error', onSocketError);
      resolve();
    };
    const onSocketError = (error: Error): void => {
      clearTimeout(timer);
      socket.off('secureConnect', onSecureConnect);
      reject(new Error(`[provider-430] provider connect failed (names only): ${error.name}`));
    };
    socket.once('secureConnect', onSecureConnect);
    socket.once('error', onSocketError);
  });

  // Every exit path closes the one connection: success, 502 fail-fast,
  // auth failure, timeout. No path holds the socket.
  try {
    await connected;

    const failOn502 = (line: string, step: string): void => {
      if (isNntpCode(line, '502')) {
        throw new Error(
          `[provider-430] provider answered 502 at ${step} (too many connections): ` +
            'failing fast with no retry — the shared account keeps 8+ connections free ' +
            'for production. Close the socket and investigate concurrent provider use.',
        );
      }
    };

    const greeting = await readReplyLine(socket, buffer, 'provider-430-greeting');
    failOn502(greeting, 'greeting');
    if (!isNntpCode(greeting, '200') && !isNntpCode(greeting, '201')) {
      throw new Error('[provider-430] unexpected provider greeting (code only, no content).');
    }

    socket.write(`AUTHINFO USER ${endpoint.user}\r\n`);
    const userReply = await readReplyLine(socket, buffer, 'provider-430-auth-user');
    failOn502(userReply, 'AUTHINFO USER');
    if (!isNntpCode(userReply, '381') && !isNntpCode(userReply, '281')) {
      throw new Error('[provider-430] provider rejected AUTHINFO USER (code only, no content).');
    }
    if (isNntpCode(userReply, '381')) {
      socket.write(`AUTHINFO PASS ${password}\r\n`);
      const passReply = await readReplyLine(socket, buffer, 'provider-430-auth-pass');
      failOn502(passReply, 'AUTHINFO PASS');
      if (!isNntpCode(passReply, '281')) {
        throw new Error('[provider-430] provider authentication failed (code only, no content).');
      }
    }

    socket.write(`STAT ${probeId}\r\n`);
    const statReply = await readReplyLine(socket, buffer, 'provider-430-stat');
    failOn502(statReply, 'STAT');
    expect(
      isNntpCode(statReply, '430'),
      '[provider-430] expected 430 (no such article) for the never-posted probe ID.',
    ).toBe(true);

    socket.write('QUIT\r\n');
    await readReplyLine(socket, buffer, 'provider-430-quit').catch(() => 'quit-unconfirmed');
  } finally {
    socket.destroy();
    peakConnections = Math.max(peakConnections, 1);
  }

  // Counts-only evidence: peak connections + bytes fetched. Never the probe
  // ID, credentials, host, or any content.
  const evidence = {
    peakConnections,
    bytesFetched: buffer.bytes,
    statVerdict: '430',
    budgetMaxConnections: budget.maxConnections,
  };
  await testInfo.attach('provider-430-control-counts', {
    body: JSON.stringify(evidence, null, 2),
    contentType: 'application/json',
  });
  expect(
    peakConnections <= budget.maxConnections,
    `[provider-430] opened ${peakConnections} connections against a budget of ${budget.maxConnections}.`,
  ).toBe(true);
});
