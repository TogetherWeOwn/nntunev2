import { readyResponse } from '#lib/ready.js';

import type { RequestHandler } from './$types';

// Stateless frontend: ready while it can serve traffic. The 503 shape
// (notReadyResponse + Retry-After, see src/lib/ready.ts) is pinned by
// src/lib/ready.test.ts for the first downstream dependency check.
export const GET: RequestHandler = () => readyResponse();
