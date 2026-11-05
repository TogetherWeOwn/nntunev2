/**
 * Recent is client-rendered on purpose (`ssr = false`): recent searches
 * are device-local state that only exists in the browser. Same discipline
 * as the library page loader.
 */

export const ssr = false;
