/**
 * Cloudflare Access service-token handling for staging.
 *
 * Staging sits behind Cloudflare Access; CI authenticates with a QA service
 * token passed as the CF-Access-Client-Id / CF-Access-Client-Secret headers.
 * Only secret NAMES appear here — values come from the environment
 * (GitHub Actions secrets on CI, shell exports locally). Nothing here may
 * log or persist a secret value.
 */

export interface WallProbe {
  status: number;
  headers: Record<string, string>;
  bodyText: string;
  url: string;
}

/** Extra headers to attach to every staging request (page + API). */
export function accessHeaders(): Record<string, string> {
  const id = process.env.CF_ACCESS_CLIENT_ID;
  const secret = process.env.CF_ACCESS_CLIENT_SECRET;
  if (id && secret) {
    return {
      'CF-Access-Client-Id': id,
      'CF-Access-Client-Secret': secret,
    };
  }
  if ((id && !secret) || (!id && secret)) {
    throw new Error(
      'Cloudflare Access misconfigured: set both CF_ACCESS_CLIENT_ID and ' +
        'CF_ACCESS_CLIENT_SECRET, or neither (secret names only — never values).',
    );
  }
  return {};
}

/** Bearer header for the staging test-account seam, when configured. */
export function qaAuthHeader(): Record<string, string> {
  const token = process.env.QA_AUTH_TOKEN;
  return token ? { Authorization: `Bearer ${token}` } : {};
}

/**
 * Fail closed when a response is the Cloudflare Access wall instead of the
 * app: a 401/403/302 that carries Access markers means the service token is
 * missing or wrong. Never silently treat the wall as an app response.
 */
export function assertNoAccessWall(probe: WallProbe, context: string): void {
  const headerDump = Object.entries(probe.headers)
    .map(([k, v]) => `${k}: ${v}`)
    .join('\n');
  const looksLikeWall =
    probe.status === 401 ||
    probe.status === 403 ||
    /cf-access|cloudflare access|cf_clearance/i.test(headerDump) ||
    /<title>\s*Access/i.test(probe.bodyText) ||
    /CF_Authorization|cf-authorization/i.test(probe.bodyText);
  if (looksLikeWall) {
    throw new Error(
      `[${context}] hit the Cloudflare Access wall at ${probe.url} ` +
        `(HTTP ${probe.status}). Set CF_ACCESS_CLIENT_ID + ` +
        'CF_ACCESS_CLIENT_SECRET for a QA service token with staging access. ' +
        'See e2e/README.md. No secret values are logged here by design.',
    );
  }
}

/**
 * Resolve the staging base URL.
 * - Returns null when unconfigured: callers skip with a notice (pre-deploy).
 * - Throws when `required` and unconfigured: callers fail closed (post-deploy).
 */
export function stagingBaseUrl(required: 'skip' | 'fail-closed', context: string): string | null {
  const raw = (process.env.STAGING_BASE_URL || '').trim().replace(/\/+$/, '');
  if (!raw) {
    if (required === 'fail-closed') {
      throw new Error(
        `[${context}] STAGING_BASE_URL is not set. Staging is expected to be ` +
          'configured — refusing to pass silently. See e2e/README.md.',
      );
    }
    return null;
  }
  if (!/^https:\/\//i.test(raw)) {
    throw new Error(
      `[${context}] STAGING_BASE_URL must be an https:// URL, got a non-https value. ` +
        'Refusing to run against a non-TLS staging target.',
    );
  }
  return raw;
}

/** Synthetic NNTP endpoint the BYOP wizard points at (hosted fixture server or test provider). */
export interface NntpEndpoint {
  host: string;
  port: number;
  user: string;
  tls: boolean;
}

/**
 * NNTP endpoint config. Host/user have fixture defaults (not secrets); the
 * password has NO default and stays in the environment. TLS defaults on.
 */
export function syntheticNntpEndpoint(): NntpEndpoint & { hasPassword: boolean } {
  const host = (process.env.SYNTHETIC_NNTP_HOST || 'synthetic-nntp').trim();
  const port = Number(process.env.SYNTHETIC_NNTP_PORT || '563');
  const user = (process.env.SYNTHETIC_NNTP_USER || 'e2e').trim();
  const tls = (process.env.SYNTHETIC_NNTP_TLS || 'on').toLowerCase() !== 'off';
  if (!host) throw new Error('[byop] SYNTHETIC_NNTP_HOST is empty.');
  if (!Number.isInteger(port) || port <= 0 || port > 65535) {
    throw new Error('[byop] SYNTHETIC_NNTP_PORT must be a valid TCP port number.');
  }
  return { host, port, user, tls, hasPassword: Boolean(process.env.SYNTHETIC_NNTP_PASS) };
}
