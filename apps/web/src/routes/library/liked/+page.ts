/**
 * Liked is client-rendered on purpose (`ssr = false`): it joins the
 * session-gated library API with device-local favorites, both of which
 * only exist in the browser. Same discipline as the library page loader.
 */

export const ssr = false;
