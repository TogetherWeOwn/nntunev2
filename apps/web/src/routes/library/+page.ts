/**
 * Library page loader. Client-rendered on purpose (`ssr = false`): the page
 * is session-gated, and the stale-snapshot fallback lives in tab-scoped
 * `sessionStorage`, which only exists in the browser. The component fetches `/api/library`
 * itself so an API outage can fall back to the cached snapshot instead of
 * rendering a blank list.
 */

export const ssr = false;
