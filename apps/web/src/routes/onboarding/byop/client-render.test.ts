import { describe, expect, it } from 'vitest';
import * as pageModule from './+page';

/**
 * The BYOP forms carry named credential inputs (`password`, the indexer
 * API key) without `method`/`action`, so a server-rendered copy submitted
 * before hydration would be a native GET with those values in the query
 * string. The page must stay client-rendered so the forms only ever exist
 * with their `preventDefault` submit handler attached.
 */
describe('onboarding/byop page options', () => {
	it('disables SSR so credential forms never render without their submit handler', () => {
		expect(pageModule.ssr).toBe(false);
	});
});
