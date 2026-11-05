// One-time authorized ownership migration: supply only the router URL to the
// actual layout. No player, queue, identity or auth behavior is substituted.
// Other Builder changes to the protected fixture remain prohibited.
export const page = { url: new URL('/library', window.location.origin) };
