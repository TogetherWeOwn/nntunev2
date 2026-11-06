/**
 * Home is client-rendered on purpose (`ssr = false`): it reads the
 * session-gated library API and device-local favorites/recent searches,
 * all of which only exist in the browser. Same discipline as the library
 * page loader.
 */

export const ssr = false;
