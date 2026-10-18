/**
 * Deployed-build identity for exact-head staging evidence.
 *
 * `BUILD_SHA` is the 40-char lowercase hex commit SHA baked at build time
 * (see `vite.config.ts` `__BUILD_SHA__` define, sourced from `BUILD_SHA` /
 * `GITHUB_SHA` at build time), or the explicit string `"unknown"` when no
 * SHA was available (dev server, preview without env, misconfigured CI).
 *
 * This module is intentionally narrow: exactly one value plus two pure
 * validators. It carries no host, path, or credential content.
 */
const SHA40 = /^[0-9a-f]{40}$/;

export function parseBuildSha(value: unknown): string {
	return typeof value === 'string' && SHA40.test(value) ? value : 'unknown';
}

export function isBuildShaKnown(sha?: string): boolean {
	return typeof sha === 'string' && SHA40.test(sha);
}

declare const __BUILD_SHA__: string | undefined;

const baked: unknown =
	typeof __BUILD_SHA__ !== 'undefined' ? __BUILD_SHA__ : 'unknown';

/** 40-hex commit SHA, or `"unknown"` when the build carried no identity. */
export const BUILD_SHA: string = parseBuildSha(baked);
