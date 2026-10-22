import { test, expect, chromium, type Browser, type Page } from '@playwright/test';
import { createServer, type IncomingMessage, type Server, type ServerResponse } from 'node:http';
import type { AddressInfo } from 'node:net';
import { syntheticNntpEndpoint } from '../helpers/access.js';
import { completeByopWizard } from '../helpers/accounts.js';

// BYOP transport-fidelity acceptance tests (offline, mocked browser page).
//
// Contract under test: the BYOP wizard helper must fail CLOSED when it
// cannot make the page match the synthetic fixture transport, instead of
// silently submitting whatever the page defaults to.
//
// - syntheticNntpEndpoint() only accepts the ports the real BYOP UI/API
//   allowlist offers (119 / 563). Any other valid TCP port is a config
//   error, named before any page interaction — never a selectOption
//   timeout deep in the wizard.
// - TLS-off fixture config is rejected before interaction. The product
//   only allows plaintext behind an explicit in-page acknowledgement;
//   the fixture env has no such opt-in, so the harness must not enable
//   (or silently ignore) plaintext on the fixture's behalf.
// - For a supported config, completeByopWizard explicitly sets AND
//   verifies the TLS checkbox's requested state. A missing, unusable or
//   un-settable TLS control fails with a names-only diagnostic BEFORE
//   any test-connection / save submission: zero unintended submits.
// - The supported transports keep working: default TLS-on/563, and
//   119 selected explicitly (the allowlist is not narrowed to 563).
//
// The browser cases drive the real completeByopWizard against a local
// fake BYOP page (same data-testid contract as the app) in Chromium, and
// assert on the submissions the fake page actually received — not on
// helper internals. No staging, no network beyond 127.0.0.1, no secrets
// (the dummy password below is a canary for names-only diagnostics).

const DUMMY_PASS = 'dummy-byop-pass-canary';

async function withEnvAsync(
  vars: Record<string, string | undefined>,
  fn: () => Promise<void>,
): Promise<void> {
  const saved: Record<string, string | undefined> = {};
  for (const key of Object.keys(vars)) {
    saved[key] = process.env[key];
    if (vars[key] === undefined) delete process.env[key];
    else process.env[key] = vars[key];
  }
  try {
    await fn();
  } finally {
    for (const key of Object.keys(vars)) {
      if (saved[key] === undefined) delete process.env[key];
      else process.env[key] = saved[key];
    }
  }
}

function withEnv(vars: Record<string, string | undefined>, fn: () => void): void {
  const saved: Record<string, string | undefined> = {};
  for (const key of Object.keys(vars)) {
    saved[key] = process.env[key];
    if (vars[key] === undefined) delete process.env[key];
    else process.env[key] = vars[key];
  }
  try {
    fn();
  } finally {
    for (const key of Object.keys(vars)) {
      if (saved[key] === undefined) delete process.env[key];
      else process.env[key] = saved[key];
    }
  }
}

const fixtureEnv = (overrides: Record<string, string | undefined> = {}) => ({
  SYNTHETIC_NNTP_HOST: 'synthetic-nntp',
  SYNTHETIC_NNTP_USER: 'e2e',
  SYNTHETIC_NNTP_PASS: DUMMY_PASS,
  SYNTHETIC_NNTP_PORT: '563',
  SYNTHETIC_NNTP_TLS: 'on',
  ...overrides,
});

test.describe('syntheticNntpEndpoint transport allowlist', () => {
  for (const port of ['8080', '443', '25', '118', '564', '65535']) {
    test(`rejects unsupported port ${port} naming the allowlist`, () => {
      withEnv(fixtureEnv({ SYNTHETIC_NNTP_PORT: port }), () => {
        let message = '';
        try {
          syntheticNntpEndpoint();
        } catch (error) {
          message = (error as Error).message;
        }
        expect(message, `port ${port} was silently accepted`).toMatch(/SYNTHETIC_NNTP_PORT/);
        expect(message).toContain('119');
        expect(message).toContain('563');
        expect(message).not.toContain(DUMMY_PASS);
      });
    });
  }

  test('accepts the supported ports 563 and 119 with TLS on (positive control)', () => {
    withEnv(fixtureEnv({ SYNTHETIC_NNTP_PORT: '563' }), () => {
      expect(syntheticNntpEndpoint()).toMatchObject({ port: 563, tls: true });
    });
    withEnv(fixtureEnv({ SYNTHETIC_NNTP_PORT: '119' }), () => {
      expect(syntheticNntpEndpoint()).toMatchObject({ port: 119, tls: true });
    });
  });

  test('defaults to 563 with TLS on (positive control)', () => {
    withEnv(
      fixtureEnv({ SYNTHETIC_NNTP_PORT: undefined, SYNTHETIC_NNTP_TLS: undefined }),
      () => {
        expect(syntheticNntpEndpoint()).toMatchObject({ port: 563, tls: true });
      },
    );
  });

  test('rejects TLS-off fixture config instead of enabling plaintext', () => {
    withEnv(fixtureEnv({ SYNTHETIC_NNTP_PORT: '119', SYNTHETIC_NNTP_TLS: 'off' }), () => {
      let message = '';
      try {
        syntheticNntpEndpoint();
      } catch (error) {
        message = (error as Error).message;
      }
      expect(message, 'TLS-off config was silently accepted').toMatch(
        /SYNTHETIC_NNTP_TLS|TLS|plaintext/i,
      );
      expect(message).not.toContain(DUMMY_PASS);
    });
  });

  test('rejects TLS-off even on port 563 (no silent TLS-on fallback)', () => {
    withEnv(fixtureEnv({ SYNTHETIC_NNTP_PORT: '563', SYNTHETIC_NNTP_TLS: 'off' }), () => {
      expect(() => syntheticNntpEndpoint()).toThrow(/SYNTHETIC_NNTP_TLS|TLS|plaintext/i);
    });
  });
});

type TlsVariant = 'checked' | 'unchecked' | 'missing' | 'not-checkbox' | 'reverting';

interface FakeByop {
  baseURL: string;
  pageHits: number;
  testHits: Array<{ tls: string; port: string }>;
  saveHits: number;
  close: () => Promise<void>;
}

function tlsControlHtml(variant: TlsVariant): string {
  switch (variant) {
    case 'checked':
      return '<label><input data-testid="byop-tls" name="tls" type="checkbox" checked /> Use TLS</label>';
    case 'unchecked':
      return '<label><input data-testid="byop-tls" name="tls" type="checkbox" /> Use TLS</label>';
    case 'missing':
      return '<p>No TLS control on this page.</p>';
    case 'not-checkbox':
      return '<label>Use TLS <input data-testid="byop-tls" name="tls" type="text" value="on" /></label>';
    case 'reverting':
      // A control that refuses every attempt to change its state: clicks
      // are swallowed, so no setter can ever make it checked.
      return (
        '<label><input data-testid="byop-tls" name="tls" type="checkbox" /> Use TLS</label>' +
        "<script>document.querySelector('[data-testid=byop-tls]')" +
        ".addEventListener('click', (e) => e.preventDefault());</script>"
      );
  }
}

async function startFakeByop(variant: TlsVariant): Promise<FakeByop> {
  const state: Omit<FakeByop, 'baseURL' | 'close'> = { pageHits: 0, testHits: [], saveHits: 0 };
  const page = `<!doctype html><html><body>
    <div id="step1">
      <label>Hostname <input data-testid="byop-host" name="host" /></label>
      <label>Port <select data-testid="byop-port" name="port">
        <option value="119">119 (plaintext NNTP)</option>
        <option value="563" selected>563 (TLS)</option>
      </select></label>
      ${tlsControlHtml(variant)}
      <label>Username <input data-testid="byop-user" name="username" /></label>
      <label>Password <input data-testid="byop-pass" name="password" type="password" /></label>
      <button type="button" onclick="show('step2')">Continue</button>
    </div>
    <div id="step2" style="display:none">
      <p>Optional indexer.</p>
      <button type="button" onclick="show('step3')">Continue</button>
    </div>
    <div id="step3" style="display:none">
      <button type="button" data-testid="byop-test-connection" onclick="runTest()">Test connection</button>
      <p data-testid="byop-status" style="display:none">connected ok</p>
      <button type="button" data-testid="byop-save" style="display:none" onclick="save()">Save</button>
    </div>
    <script>
      function show(id) {
        for (const s of ['step1','step2','step3']) {
          document.getElementById(s).style.display = s === id ? 'block' : 'none';
        }
      }
      async function runTest() {
        const tls = document.querySelector('[data-testid=byop-tls]');
        const port = document.querySelector('[data-testid=byop-port]');
        await fetch('/__hit/test?tls=' + encodeURIComponent(tls ? String(!!tls.checked) : 'absent')
          + '&port=' + encodeURIComponent(port ? port.value : 'absent'), { method: 'POST' });
        document.querySelector('[data-testid=byop-status]').style.display = 'block';
        document.querySelector('[data-testid=byop-save]').style.display = 'block';
      }
      async function save() {
        await fetch('/__hit/save', { method: 'POST' });
        location.href = '/library';
      }
    </script>
  </body></html>`;
  const server: Server = createServer((req: IncomingMessage, res: ServerResponse) => {
    const url = new URL(req.url ?? '/', 'http://127.0.0.1');
    if (url.pathname === '/onboarding/byop') {
      state.pageHits += 1;
      res.writeHead(200, { 'content-type': 'text/html' });
      res.end(page);
      return;
    }
    if (url.pathname === '/__hit/test') {
      state.testHits.push({
        tls: url.searchParams.get('tls') ?? '',
        port: url.searchParams.get('port') ?? '',
      });
      res.writeHead(204);
      res.end();
      return;
    }
    if (url.pathname === '/__hit/save') {
      state.saveHits += 1;
      res.writeHead(204);
      res.end();
      return;
    }
    res.writeHead(200, { 'content-type': 'text/html' });
    res.end('<!doctype html><html><body>library</body></html>');
  });
  const port = await new Promise<number>((resolve, reject) => {
    server.on('error', reject);
    server.listen(0, '127.0.0.1', () => resolve((server.address() as AddressInfo).port));
  });
  return {
    ...state,
    // state is mutated by the handler; expose live counters via getters.
    get pageHits() {
      return state.pageHits;
    },
    get testHits() {
      return state.testHits;
    },
    get saveHits() {
      return state.saveHits;
    },
    baseURL: `http://127.0.0.1:${port}`,
    close: () => new Promise<void>((resolve) => server.close(() => resolve())),
  };
}

test.describe('completeByopWizard transport fidelity (mocked browser page)', () => {
  let browser: Browser | undefined;

  test.beforeAll(async () => {
    try {
      browser = await chromium.launch();
    } catch {
      browser = undefined; // each test skips with notice; CI installs --with-deps
    }
  });

  test.afterAll(async () => {
    await browser?.close().catch(() => {});
  });

  async function runWizard(
    variant: TlsVariant,
    env: Record<string, string | undefined>,
    setup?: (page: Page) => Promise<void>,
  ): Promise<{ fake: FakeByop; error: Error | null }> {
    if (!browser) {
      test.skip(true, 'Chromium unavailable in this container; runs in CI with --with-deps.');
      throw new Error('unreachable');
    }
    const fake = await startFakeByop(variant);
    const context = await browser.newContext();
    let error: Error | null = null;
    try {
      const page = await context.newPage();
      if (setup) await setup(page);
      await withEnvAsync(env, async () => {
        try {
          await completeByopWizard(page, fake.baseURL);
        } catch (caught) {
          error = caught as Error;
        }
      });
    } finally {
      await context.close().catch(() => {});
    }
    return { fake, error };
  }

  test('default TLS-on/563 completes and submits with TLS on (positive control)', async () => {
    const { fake, error } = await runWizard('checked', fixtureEnv());
    try {
      expect(error?.message ?? null).toBeNull();
      expect(fake.testHits).toEqual([{ tls: 'true', port: '563' }]);
      expect(fake.saveHits).toBe(1);
    } finally {
      await fake.close();
    }
  });

  test('explicitly sets TLS on when the checkbox starts unchecked (opposite state)', async () => {
    const { fake, error } = await runWizard('unchecked', fixtureEnv());
    try {
      expect(error?.message ?? null).toBeNull();
      expect(fake.testHits).toEqual([{ tls: 'true', port: '563' }]);
      expect(fake.saveHits).toBe(1);
    } finally {
      await fake.close();
    }
  });

  test('port 119 selected explicitly keeps working with TLS on (allowlist not narrowed)', async () => {
    const { fake, error } = await runWizard('checked', fixtureEnv({ SYNTHETIC_NNTP_PORT: '119' }));
    try {
      expect(error?.message ?? null).toBeNull();
      expect(fake.testHits).toEqual([{ tls: 'true', port: '119' }]);
      expect(fake.saveHits).toBe(1);
    } finally {
      await fake.close();
    }
  });

  test('missing TLS control fails before any test/save submission', async () => {
    const { fake, error } = await runWizard('missing', fixtureEnv());
    try {
      expect(error, 'wizard submitted without a TLS control to verify').not.toBeNull();
      expect(error?.message).toMatch(/TLS/i);
      expect(error?.message).not.toContain(DUMMY_PASS);
      expect(fake.testHits).toEqual([]);
      expect(fake.saveHits).toBe(0);
    } finally {
      await fake.close();
    }
  });

  test('unusable TLS control (not a checkbox) fails before any test/save submission', async () => {
    const { fake, error } = await runWizard('not-checkbox', fixtureEnv());
    try {
      expect(error, 'wizard submitted past a non-checkbox TLS control').not.toBeNull();
      expect(error?.message).toMatch(/TLS/i);
      expect(error?.message).not.toContain(DUMMY_PASS);
      expect(fake.testHits).toEqual([]);
      expect(fake.saveHits).toBe(0);
    } finally {
      await fake.close();
    }
  });

  test('setter failure (control never becomes checked) fails before any test/save submission', async () => {
    const { fake, error } = await runWizard('reverting', fixtureEnv(), async (page) => {
      // Keep a swallowed-setter implementation from hiding behind the
      // default action timeout; the fake page is local and instant.
      page.setDefaultTimeout(2_000);
    });
    try {
      expect(error, 'wizard submitted although TLS never became checked').not.toBeNull();
      expect(error?.message).toMatch(/TLS|checkbox|checked/i);
      expect(error?.message).not.toContain(DUMMY_PASS);
      expect(fake.testHits).toEqual([]);
      expect(fake.saveHits).toBe(0);
    } finally {
      await fake.close();
    }
  });

  test('unsupported port fails with a config diagnostic, not a select timeout', async () => {
    const { fake, error } = await runWizard(
      'checked',
      fixtureEnv({ SYNTHETIC_NNTP_PORT: '8080' }),
      async (page) => {
        page.setDefaultTimeout(2_000);
      },
    );
    try {
      expect(error, 'wizard accepted an unsupported fixture port').not.toBeNull();
      expect(error?.message).toMatch(/SYNTHETIC_NNTP_PORT|119|563/);
      expect(error?.message).not.toMatch(/Timeout \d+ms exceeded/);
      expect(error?.message).not.toContain(DUMMY_PASS);
      expect(fake.testHits).toEqual([]);
      expect(fake.saveHits).toBe(0);
    } finally {
      await fake.close();
    }
  });

  test('TLS-off fixture config is rejected before any page interaction', async () => {
    const { fake, error } = await runWizard(
      'checked',
      fixtureEnv({ SYNTHETIC_NNTP_PORT: '119', SYNTHETIC_NNTP_TLS: 'off' }),
    );
    try {
      expect(error, 'wizard ran with a TLS-off fixture config').not.toBeNull();
      expect(error?.message).toMatch(/SYNTHETIC_NNTP_TLS|TLS|plaintext/i);
      expect(error?.message).not.toContain(DUMMY_PASS);
      expect(fake.pageHits).toBe(0);
      expect(fake.testHits).toEqual([]);
      expect(fake.saveHits).toBe(0);
    } finally {
      await fake.close();
    }
  });
});
