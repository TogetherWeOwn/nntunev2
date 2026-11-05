/**
 * Search is client-rendered on purpose (`ssr = false`): it filters the
 * session-gated library API, which only exists in the browser. Same
 * discipline as the library page loader.
 */

export const ssr = false;
