/**
 * Client-rendered on purpose (`ssr = false`), like the other session-gated
 * pages. The provider and indexer forms have named credential inputs and
 * rely on an `onsubmit` handler to stop native submission. A server-rendered
 * copy of those forms exists before that handler is attached, so pressing
 * Enter before hydration (slow network, blocked bundle) would do a native
 * GET and put the provider password or indexer API key in the URL, browser
 * history and request logs. Rendering in the browser only means the forms
 * never exist without their handler.
 */
export const ssr = false;
