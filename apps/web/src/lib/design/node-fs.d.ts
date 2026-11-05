// Test-only ambient types for the single node builtin the design tests use.
//
// The token tests must read the shipped `tokens.css` bytes, but this
// workspace's Vite 8 pipeline resolves `?raw`/`?inline` CSS imports to an
// empty string, so the Vite-glob convention other tests use cannot see the
// stylesheet. Vitest runs in node, so `node:fs` works at runtime; this
// declaration covers `svelte-check` only. The web package carries no
// @types/node by convention, so there is nothing to conflict with — keep
// this file scoped to exactly what the tests call.
declare module 'node:fs' {
	export function readFileSync(path: URL, encoding: 'utf8'): string;
}
