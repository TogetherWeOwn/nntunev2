// One-time authorized ownership migration: supply only the router URL to the
// actual layout. No player, queue, identity or auth behavior is substituted.
// Other Builder changes to the protected fixture remain prohibited.
import { route } from './route.svelte';

export const page = { get url() { return route.url; } };
