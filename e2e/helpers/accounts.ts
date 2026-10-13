import type { APIRequestContext } from '@playwright/test';
import { API, uniqueTestEmail } from './contract.js';
import { accessHeaders, qaAuthHeader } from './access.js';

export interface TestAccount {
  email: string;
  /** Session cookie/header value when the seam provisions a session directly. */
  session?: string;
  /** True when minted via the API seam (skip the UI signup form). */
  viaApi: boolean;
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
