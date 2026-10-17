import { test, expect } from '@playwright/test';
import { assertNoAccessWall, stagingBaseUrl } from '../helpers/access.js';
import { applyAccountToPage } from '../helpers/accounts.js';

// Staging security-headers probe: the offline pin
// (apps/web/tests/security-headers.spec.ts) proves the app emits the headers,
// but staging (or Access) could strip/override them undetected. This spec
// asserts the live app-shell document response still carries them.
//
// File-level skip: pre-deploy (no STAGING_BASE_URL) the whole file skips
// without launching a browser, so `npm run e2e:staging` stays green and fast
// until staging exists. In-test guards below remain as defense in depth.
test.skip(
  !process.env.STAGING_BASE_URL,
  'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.',
);

const requiredHeaders: Record<string, string> = {
  'strict-transport-security': 'max-age=31536000; includeSubDomains',
  'x-content-type-options': 'nosniff',
  'x-frame-options': 'DENY',
  'referrer-policy': 'no-referrer',
};

test.describe('staging security response headers', () => {
  test('GET / serves the app shell with security headers and a nonce-bearing CSP', async ({
    page,
  }) => {
    const baseURL = stagingBaseUrl('skip', 'security-headers');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    // Browser loads carry the Access pair via the staging-origin route
    // (no context-level headers by design — see applyAccountToPage).
    await applyAccountToPage(page, null, baseURL);
    await page.addInitScript(() => {
      window.addEventListener('securitypolicyviolation', (event) => {
        const violations = ((window as Window & { __cspViolations?: string[] }).__cspViolations ??=
          []);
        violations.push(`${event.violatedDirective}: ${event.blockedURI}`);
      });
    });

    const response = await page.goto(baseURL, { waitUntil: 'domcontentloaded', timeout: 30_000 });
    expect(response, 'no HTTP response for app shell').not.toBeNull();
    const bodyText = await page.content();
    const headers: Record<string, string> = {};
    for (const [k, v] of Object.entries(response?.headers() ?? {})) headers[k] = v;
    assertNoAccessWall(
      { status: response?.status() ?? 0, headers, bodyText, url: baseURL },
      'security-headers',
    );
    expect(response?.status(), 'app shell did not return 200 — is the frontend deployed?').toBe(
      200,
    );

    for (const [name, value] of Object.entries(requiredHeaders)) {
      expect(headers[name], `app shell should include ${name}`).toBe(value);
    }

    const permissionsPolicy = headers['permissions-policy'];
    expect(permissionsPolicy, 'app shell should include permissions-policy').toBeTruthy();
    expect(permissionsPolicy).toContain('camera=()');
    expect(permissionsPolicy).toContain('microphone=()');
    expect(permissionsPolicy).toContain('geolocation=()');
    expect(permissionsPolicy).not.toMatch(/publickey-credentials-(?:get|create)/);

    const csp = headers['content-security-policy'];
    expect(csp, 'app shell should include content-security-policy').toBeTruthy();
    // Nonce-bearing: SvelteKit nonce mode emits per-request 'nonce-…' sources
    // so inline scripts need neither unsafe-inline nor unsafe-eval. A static
    // CSP without a nonce means the nonce pipeline was stripped upstream.
    expect(csp, 'app shell CSP should be nonce-bearing').toMatch(/nonce-[A-Za-z0-9+/=]+/);
    const directives = new Map(
      csp!.split(';').map((directive) => {
        const [name, ...sources] = directive.trim().split(/\s+/);
        return [name, sources];
      }),
    );
    expect(directives.get('default-src')).toContain("'self'");
    expect(directives.get('object-src')).toContain("'none'");
    expect(directives.get('base-uri')).toContain("'self'");
    expect(directives.get('form-action')).toContain("'self'");
    expect(directives.get('frame-ancestors')).toContain("'none'");
    expect(directives.get('script-src')).toBeDefined();
    expect(directives.get('script-src')).not.toContain("'unsafe-inline'");
    expect(directives.get('script-src')).not.toContain("'unsafe-eval'");

    expect(
      await page.evaluate(
        () => (window as Window & { __cspViolations?: string[] }).__cspViolations ?? [],
      ),
      'app shell should not cause a CSP violation',
    ).toEqual([]);
  });
});
