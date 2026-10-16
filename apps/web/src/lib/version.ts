import pkg from '../../package.json';

/**
 * Public app version served by `GET /api/health`.
 *
 * `package.json` is the single source of truth — never a commit SHA,
 * hostname, or other internal identifier (see version.test.ts).
 */
export const APP_VERSION: string = pkg.version;
