import type { APIRequestContext, Page } from '@playwright/test';
import { API, uniqueTestEmail } from './contract.js';
import { accessHeaders, qaAuthHeader } from './access.js';

export interface TestAccount {
  email: string;
  /**
   * Opaque session for the minted account, when the seam provisions one.
   * Contract (owned by the auth card): the staging API accepts it as
   * `Authorization: Bearer <session>`, taking precedence over QA_AUTH_TOKEN.
   * If the auth card implements cookie sessions instead, update
   * accountHeaders/applyAccountToPage together with the specs.
   */
  session?: string;
  /** True when minted via the API seam (skip the UI signup form). */
  viaApi: boolean;
}

/**
 * Headers for API calls as a specific test account: Access pair +
 * per-account session (preferred) falling back to the QA seam token.
 */
export function accountHeaders(account: TestAccount | null): Record<string, string> {
  const headers: Record<string, string> = { ...accessHeaders(), ...qaAuthHeader() };
  if (account?.session) headers.Authorization = `Bearer ${account.session}`;
  return headers;
}

/**
 * Authenticate a browser page as a minted account. Call BEFORE the first
 * navigation: page-level headers apply to subsequent loads, never past ones.
 * Access headers are repeated here so a page-level override cannot drop them
 * (the context-level config already sets them; this is belt and braces).
 */
export async function applyAccountToPage(
  page: Page,
  account: TestAccount | null,
): Promise<void> {
  const headers: Record<string, string> = { ...accessHeaders() };
  if (account?.session) headers.Authorization = `Bearer ${account.session}`;
  if (Object.keys(headers).length > 0) {
    await page.setExtraHTTPHeaders(headers);
  }
}

/**
 * Mint a throwaway staging account via POST /api/test-accounts.
 *
 * Returns null when the seam is unavailable (no QA_AUTH_TOKEN, endpoint not
 * implemented yet, or non-2xx) so callers fall back to UI signup. Never
 * throws for seam absence — only for transport-level failures. Staging-only:
 * this endpoint must never exist in production.
 */
export async function tryMintTestAccount(
  request: APIRequestContext,
  baseURL: string,
  plan?: string,
): Promise<TestAccount | null> {
  if (!process.env.QA_AUTH_TOKEN) return null;
  let response;
  try {
    response = await request.post(`${baseURL}${API.testAccounts}`, {
      headers: { ...accessHeaders(), ...qaAuthHeader(), 'Content-Type': 'application/json' },
      data: { email: uniqueTestEmail(plan ? `e2e-${plan}` : 'e2e'), ...(plan ? { plan } : {}) },
      timeout: 30_000,
    });
  } catch {
    return null;
  }
  if (!response.ok()) return null;
  let body: Record<string, unknown>;
  try {
    body = (await response.json()) as Record<string, unknown>;
  } catch {
    return null;
  }
  const email = typeof body.email === 'string' ? body.email : uniqueTestEmail('e2e');
  const session =
    typeof body.session === 'string'
      ? body.session
      : typeof body.token === 'string'
        ? body.token
        : undefined;
  return { email, session, viaApi: true };
}
